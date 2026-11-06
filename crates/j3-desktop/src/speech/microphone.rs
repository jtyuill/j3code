//! This machine's default microphone through cpal (CoreAudio on macOS, ALSA
//! on Linux, where PipeWire and PulseAudio serve ALSA clients). The stream
//! lives on a thread of its own, since cpal streams are not `Send` on every
//! platform.

use std::{
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};

use cpal::{
    FromSample, SampleFormat, SizedSample,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};

use super::{AudioSource, AudioStream};

pub struct Microphone;

impl AudioSource for Microphone {
    fn open(&self, sink: Arc<Mutex<Vec<f32>>>) -> Result<Box<dyn AudioStream>, String> {
        let (opened, opening) = mpsc::channel();
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("j3-microphone".into())
            .spawn(move || match start(sink) {
                Ok((stream, rate)) => {
                    let _ = opened.send(Ok(rate));
                    // Records until the handle is dropped.
                    let _ = stopped.recv();
                    drop(stream);
                }
                Err(e) => {
                    let _ = opened.send(Err(e));
                }
            })
            .map_err(|e| format!("starting the microphone thread: {e}"))?;
        let rate = opening
            .recv()
            .map_err(|_| "the microphone thread exited".to_owned())??;
        Ok(Box::new(Recording {
            rate,
            stop: Some(stop),
            thread: Some(thread),
        }))
    }
}

struct Recording {
    rate: u32,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioStream for Recording {
    fn rate(&self) -> u32 {
        self.rate
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens the default input device and starts appending its audio, mixed to
/// mono, to `sink`. Returns the playing stream and its sample rate.
fn start(sink: Arc<Mutex<Vec<f32>>>) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no microphone found".to_owned())?;
    let supported = device
        .default_input_config()
        .map_err(|e| format!("reading the microphone's format: {e}"))?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let rate = config.sample_rate;
    let channels = usize::from(config.channels).max(1);
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, channels, sink),
        SampleFormat::I16 => build::<i16>(&device, &config, channels, sink),
        SampleFormat::I32 => build::<i32>(&device, &config, channels, sink),
        SampleFormat::U16 => build::<u16>(&device, &config, channels, sink),
        SampleFormat::I8 => build::<i8>(&device, &config, channels, sink),
        SampleFormat::U8 => build::<u8>(&device, &config, channels, sink),
        other => {
            return Err(format!(
                "the microphone's {other} samples are not supported"
            ));
        }
    }
    .map_err(|e| format!("opening the microphone: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("starting the microphone: {e}"))?;
    Ok((stream, rate))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    sink: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream(
        *config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut sink = sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sink.extend(data.chunks(channels).map(|frame| {
                frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / frame.len() as f32
            }));
        },
        |e| tracing::warn!("microphone: {e}"),
        None,
    )
}
