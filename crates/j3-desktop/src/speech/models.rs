//! The Whisper models dictation can run, which one suits this machine, and
//! fetching one: whisper.cpp's GGML files from Hugging Face, checked against
//! their published SHA-256 before they are used.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest as _, Sha256};

/// Where whisper.cpp publishes its converted models.
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

/// A Whisper model. All transcribe English only: the `.en` checkpoints are
/// English-only, and Large v3 Turbo is held to English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpeechModel {
    BaseEn,
    SmallEn,
    MediumEn,
    LargeV3Turbo,
}

impl SpeechModel {
    pub const ALL: [Self; 4] = [
        Self::BaseEn,
        Self::SmallEn,
        Self::MediumEn,
        Self::LargeV3Turbo,
    ];

    /// The stable key `desktop.json` stores.
    pub fn key(self) -> &'static str {
        match self {
            Self::BaseEn => "base.en",
            Self::SmallEn => "small.en",
            Self::MediumEn => "medium.en",
            Self::LargeV3Turbo => "large-v3-turbo",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::BaseEn => "Base",
            Self::SmallEn => "Small",
            Self::MediumEn => "Medium",
            Self::LargeV3Turbo => "Large v3 Turbo",
        }
    }

    /// The name and download size, for pickers.
    pub fn title(self) -> String {
        format!("{} · {} MB", self.label(), self.size().div_ceil(1_000_000))
    }

    fn file(self) -> &'static str {
        match self {
            Self::BaseEn => "ggml-base.en.bin",
            Self::SmallEn => "ggml-small.en.bin",
            Self::MediumEn => "ggml-medium.en-q5_0.bin",
            Self::LargeV3Turbo => "ggml-large-v3-turbo-q8_0.bin",
        }
    }

    /// The file's size in bytes, as Hugging Face lists it.
    pub fn size(self) -> u64 {
        match self {
            Self::BaseEn => 147_964_211,
            Self::SmallEn => 487_614_201,
            Self::MediumEn => 539_225_533,
            Self::LargeV3Turbo => 874_188_075,
        }
    }

    /// The file's SHA-256, as Hugging Face lists it.
    fn sha256(self) -> &'static str {
        match self {
            Self::BaseEn => "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
            Self::SmallEn => "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
            Self::MediumEn => "76733e26ad8fe1c7a5bf7531a9d41917b2adc0f20f2e4f5531688a8c6cd88eb0",
            Self::LargeV3Turbo => {
                "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1"
            }
        }
    }

    /// The model's file in `dir`.
    pub fn path(self, dir: &Path) -> PathBuf {
        dir.join(self.file())
    }

    /// Its file is in `dir`, complete.
    pub fn is_downloaded(self, dir: &Path) -> bool {
        std::fs::metadata(self.path(dir)).is_ok_and(|meta| meta.len() == self.size())
    }
}

/// What dictation can run on here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardware {
    /// The GPU whisper.cpp offloads to, if any.
    pub gpu: Option<String>,
    /// Logical CPUs.
    pub threads: usize,
}

impl Hardware {
    /// What dictation runs on, for Settings.
    pub fn describe(&self) -> String {
        match &self.gpu {
            Some(gpu) => format!("GPU: {gpu}"),
            None => format!("CPU: {} threads", self.threads),
        }
    }
}

/// The most accurate model that still transcribes a sentence in about a
/// second on `hardware`: Large v3 Turbo on a GPU, Small on a CPU with eight
/// or more threads, Base on anything smaller.
pub fn recommend(hardware: &Hardware) -> SpeechModel {
    if hardware.gpu.is_some() {
        SpeechModel::LargeV3Turbo
    } else if hardware.threads >= 8 {
        SpeechModel::SmallEn
    } else {
        SpeechModel::BaseEn
    }
}

/// `model`'s file in `dir`, downloaded first when it is missing or
/// incomplete. `progress` hears the fraction downloaded, at most once per
/// percent. The download goes to a `.part` file that only becomes the model
/// once its checksum matches.
pub async fn fetch(
    model: SpeechModel,
    dir: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<PathBuf, String> {
    let path = model.path(dir);
    if model.is_downloaded(dir) {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{MODEL_URL}{}", model.file());
    let mut response = client
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("downloading {}: {e}", model.file()))?;
    let total = response.content_length().unwrap_or(model.size()).max(1);

    let part = dir.join(format!("{}.part", model.file()));
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(&part).map_err(|e| format!("creating {}: {e}", part.display()))?,
    );
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut percent = None;
    let written: Result<(), String> = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("downloading {}: {e}", model.file()))?
        {
            hasher.update(&chunk);
            file.write_all(&chunk)
                .map_err(|e| format!("writing {}: {e}", part.display()))?;
            received += chunk.len() as u64;
            let now = (received * 100 / total).min(100);
            if percent != Some(now) {
                percent = Some(now);
                progress(received as f32 / total as f32);
            }
        }
        file.flush()
            .map_err(|e| format!("writing {}: {e}", part.display()))
    }
    .await;
    drop(file);
    let verified = written.and_then(|()| {
        let digest = format!("{:x}", hasher.finalize());
        if digest == model.sha256() {
            Ok(())
        } else {
            Err(format!("{} failed its checksum", model.file()))
        }
    });
    if let Err(e) = verified {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &path).map_err(|e| format!("saving {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gpu_gets_turbo_and_a_cpu_a_model_its_threads_can_run() {
        let gpu = Hardware {
            gpu: Some("AMD Radeon".into()),
            threads: 4,
        };
        assert_eq!(recommend(&gpu), SpeechModel::LargeV3Turbo);
        let big_cpu = Hardware {
            gpu: None,
            threads: 16,
        };
        assert_eq!(recommend(&big_cpu), SpeechModel::SmallEn);
        let small_cpu = Hardware {
            gpu: None,
            threads: 4,
        };
        assert_eq!(recommend(&small_cpu), SpeechModel::BaseEn);
    }
}
