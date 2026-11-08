//! Speech recognition with whisper.cpp, on the GPU when there is one: Metal
//! on Apple silicon, Vulkan on Linux.

use std::{
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use tokio::runtime::Handle;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use super::{
    ModelStatus, Pass, Recognizer,
    models::{self, Hardware, SpeechModel},
    resample::WHISPER_RATE,
};

/// whisper.cpp skips audio shorter than a second; shorter recordings are
/// padded with silence to this length.
const MIN_SAMPLES: usize = WHISPER_RATE as usize * 11 / 10;
/// RMS level (of full scale) some 30 ms frame must reach to count as
/// speech, well above a quiet room's hiss (about 0.004 on a desk
/// microphone); Whisper makes up words for silence.
const SPEECH_LEVEL: f32 = 0.01;

pub struct Whisper {
    models_dir: PathBuf,
    /// For downloads; the worker thread blocks on them.
    runtime: Handle,
    /// The hardware and, on Linux, the Vulkan device index to run on.
    detected: LazyLock<(Hardware, Option<i32>)>,
    loaded: Mutex<Option<(SpeechModel, WhisperState)>>,
}

impl Whisper {
    pub fn new(models_dir: PathBuf, runtime: Handle) -> Self {
        whisper_rs::install_logging_hooks();
        Self {
            models_dir,
            runtime,
            detected: LazyLock::new(detect),
            loaded: Mutex::new(None),
        }
    }
}

fn detect() -> (Hardware, Option<i32>) {
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let (gpu, device) = gpu();
    (Hardware { gpu, threads }, device)
}

/// The GPU whisper.cpp uses, and its device index.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn gpu() -> (Option<String>, Option<i32>) {
    (Some("Apple silicon (Metal)".to_owned()), None)
}

/// The first Vulkan device with room for Large v3 Turbo that is a real GPU
/// (Mesa's llvmpipe and SwiftShader are CPU renderers). ggml lists discrete
/// GPUs before integrated ones.
#[cfg(target_os = "linux")]
fn gpu() -> (Option<String>, Option<i32>) {
    const MIN_VRAM: usize = 2 << 30;
    // ggml's Vulkan backend throws a C++ exception, which aborts the process,
    // when the loader finds no driver (a machine or CI runner without a GPU);
    // ask Vulkan itself first.
    if !vulkan_has_devices() {
        return (None, None);
    }
    whisper_rs::vulkan::list_devices()
        .into_iter()
        .find(|device| {
            let name = device.name.to_lowercase();
            device.vram.total >= MIN_VRAM
                && !name.contains("llvmpipe")
                && !name.contains("swiftshader")
        })
        .map_or((None, None), |device| (Some(device.name), Some(device.id)))
}

/// Whether a Vulkan 1.2 instance can be created and sees a physical device:
/// what ggml needs before it can list devices without throwing.
#[cfg(target_os = "linux")]
fn vulkan_has_devices() -> bool {
    use ash::vk;
    // SAFETY: loads the system's Vulkan loader; `entry` outlives the instance,
    // which is destroyed before returning.
    unsafe {
        let Ok(entry) = ash::Entry::load() else {
            return false;
        };
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_2);
        let info = vk::InstanceCreateInfo::default().application_info(&app);
        let Ok(instance) = entry.create_instance(&info, None) else {
            return false;
        };
        let found = instance
            .enumerate_physical_devices()
            .is_ok_and(|devices| !devices.is_empty());
        instance.destroy_instance(None);
        found
    }
}

#[cfg(not(any(all(target_os = "macos", target_arch = "aarch64"), target_os = "linux")))]
fn gpu() -> (Option<String>, Option<i32>) {
    (None, None)
}

impl Recognizer for Whisper {
    fn hardware(&self) -> &Hardware {
        &self.detected.0
    }

    fn prepare(
        &self,
        model: SpeechModel,
        status: &mut dyn FnMut(ModelStatus),
    ) -> Result<(), String> {
        let mut loaded = self
            .loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if loaded
            .as_ref()
            .is_some_and(|(current, _)| *current == model)
        {
            return Ok(());
        }
        // Free the previous model (and its VRAM) before loading another.
        *loaded = None;
        let path = if model.is_downloaded(&self.models_dir) {
            model.path(&self.models_dir)
        } else {
            status(ModelStatus::Downloading {
                model,
                fraction: 0.,
            });
            self.runtime
                .block_on(models::fetch(model, &self.models_dir, &mut |fraction| {
                    status(ModelStatus::Downloading { model, fraction })
                }))?
        };
        status(ModelStatus::Loading(model));
        let (hardware, device) = &*self.detected;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(hardware.gpu.is_some());
        params.flash_attn(hardware.gpu.is_some());
        if let Some(device) = device {
            params.gpu_device(*device);
        }
        let context = WhisperContext::new_with_params(&path, params)
            .map_err(|e| format!("loading {}: {e}", path.display()))?;
        let state = context
            .create_state()
            .map_err(|e| format!("loading {}: {e}", path.display()))?;
        *loaded = Some((model, state));
        Ok(())
    }

    fn transcribe(&self, audio: &[f32], pass: Pass) -> Result<String, String> {
        if !has_speech(audio) {
            return Ok(String::new());
        }
        let padded;
        let audio = if audio.len() < MIN_SAMPLES {
            padded = [audio, &vec![0.; MIN_SAMPLES - audio.len()]].concat();
            &padded
        } else {
            audio
        };
        let mut loaded = self
            .loaded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_, state) = loaded
            .as_mut()
            .ok_or_else(|| "no speech model is loaded".to_owned())?;
        let mut params = FullParams::new(match pass {
            // Partials only show progress; the final pass is the text.
            Pass::Partial => SamplingStrategy::Greedy { best_of: 1 },
            Pass::Final => SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: -1.,
            },
        });
        params.set_language(Some("en"));
        params.set_n_threads(self.detected.0.threads.clamp(1, 8) as i32);
        params.set_no_context(true);
        params.set_suppress_nst(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state
            .full(params, audio)
            .map_err(|e| format!("transcribing: {e}"))?;
        let segments = state
            .as_iter()
            .map(|segment| segment.to_str_lossy().map(|text| text.into_owned()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("transcribing: {e}"))?;
        Ok(join_segments(&segments))
    }
}

/// Some 30 ms frame of `audio` is louder than [`SPEECH_LEVEL`].
fn has_speech(audio: &[f32]) -> bool {
    audio.chunks(WHISPER_RATE as usize * 3 / 100).any(|frame| {
        let power = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        power.sqrt() > SPEECH_LEVEL
    })
}

/// Whisper's segments as one line of text, without the annotations it
/// writes for what is not speech (`[BLANK_AUDIO]`, `(keyboard clicking)`).
fn join_segments(segments: &[String]) -> String {
    let words: Vec<&str> = segments
        .iter()
        .map(|segment| segment.trim())
        .filter(|segment| {
            !(segment.starts_with('[') && segment.ends_with(']')
                || segment.starts_with('(') && segment.ends_with(')'))
        })
        .flat_map(str::split_whitespace)
        .collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_join_into_one_line_without_non_speech_annotations() {
        let segments = [
            " Hello there,".to_owned(),
            "[BLANK_AUDIO]".to_owned(),
            " (keyboard clicking) ".to_owned(),
            "  how are\n you?".to_owned(),
        ];
        assert_eq!(join_segments(&segments), "Hello there, how are you?");
    }

    #[test]
    fn silence_and_hiss_are_not_speech() {
        assert!(!has_speech(&vec![0.; 16_000]));
        let hiss: Vec<f32> = (0..16_000)
            .map(|n| if n % 2 == 0 { 0.002 } else { -0.002 })
            .collect();
        assert!(!has_speech(&hiss));
        let mut spoken = hiss;
        spoken[8_000..8_480].iter_mut().for_each(|s| *s *= 20.);
        assert!(has_speech(&spoken));
    }
}
