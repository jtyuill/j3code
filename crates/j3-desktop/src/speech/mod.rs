//! Dictation: this machine's microphone transcribed on this machine, with
//! whisper.cpp, whichever environment the thread is on (the microphone is
//! the one at the keyboard).
//!
//! The microphone opens the moment dictation starts and records everything
//! until a short tail after it stops, so no word is lost to a model that is
//! still downloading or loading: the model gets ready while the user
//! speaks. While recording, the whole recording so far is transcribed again
//! whenever the last pass is done (greedy, for feedback in the draft); the
//! final transcript is one beam-search pass over all of it.
//!
//! [`SpeechEngine`] is the part off the UI thread: the audio source and a
//! worker thread that owns the recognizer. [`Speech`] is its GPUI face: it
//! owns the one capture that may run at a time and reports its progress as
//! [`SpeechEvent`]s to the composer that started it.

mod microphone;
pub mod models;
mod resample;
mod whisper;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use gpui_kit::{App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, Task};
use tokio::sync::{mpsc as async_mpsc, oneshot};

use self::{
    models::{Hardware, SpeechModel},
    resample::{Resampler, WHISPER_RATE},
};
use crate::services::Services;

/// How long the microphone keeps recording after a stop, so the last
/// syllable is not cut off.
const TAIL: Duration = Duration::from_millis(300);
/// How often a recording checks whether to transcribe what it has so far.
const TICK: Duration = Duration::from_millis(250);
/// New audio a partial transcription waits for, in 16 kHz samples.
const PARTIAL_STEP: usize = WHISPER_RATE as usize * 3 / 4;

/// Where captured audio comes from.
pub trait AudioSource: Send + Sync + 'static {
    /// Starts recording, appending mono samples at the stream's rate to
    /// `sink` until the returned stream is dropped.
    fn open(&self, sink: Arc<Mutex<Vec<f32>>>) -> Result<Box<dyn AudioStream>, String>;
}

/// A recording in progress; dropping it stops the recording.
pub trait AudioStream: Send {
    /// Samples per second.
    fn rate(&self) -> u32;
}

/// How carefully to transcribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// Quick, while the user is still speaking.
    Partial,
    /// The transcript that stays.
    Final,
}

/// Turns 16 kHz mono speech into text. Its calls block; they run on the
/// engine's worker thread, one at a time.
pub trait Recognizer: Send + Sync + 'static {
    /// What it runs on, which decides the recommended model.
    fn hardware(&self) -> &Hardware;
    /// Downloads and loads `model` unless it is loaded already, reporting
    /// progress to `status`.
    fn prepare(
        &self,
        model: SpeechModel,
        status: &mut dyn FnMut(ModelStatus),
    ) -> Result<(), String>;
    /// Transcribes `audio` with the prepared model; empty when there is no
    /// speech in it.
    fn transcribe(&self, audio: &[f32], pass: Pass) -> Result<String, String>;
}

/// Where the speech model is.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStatus {
    /// Nothing has been asked of it yet.
    Idle,
    Downloading {
        model: SpeechModel,
        /// 0 to 1.
        fraction: f32,
    },
    Loading(SpeechModel),
    Ready(SpeechModel),
    Failed(String),
}

enum Job {
    Prepare(SpeechModel),
    Transcribe {
        model: SpeechModel,
        audio: Vec<f32>,
        pass: Pass,
        reply: oneshot::Sender<Result<String, String>>,
    },
}

/// The audio source and the recognizer's worker thread.
pub struct SpeechEngine {
    source: Box<dyn AudioSource>,
    recognizer: Arc<dyn Recognizer>,
    jobs: mpsc::Sender<Job>,
    /// Taken once, by [`Speech`].
    statuses: Mutex<Option<async_mpsc::UnboundedReceiver<ModelStatus>>>,
}

impl SpeechEngine {
    pub fn new(source: Box<dyn AudioSource>, recognizer: Arc<dyn Recognizer>) -> Arc<Self> {
        let (jobs, queue) = mpsc::channel();
        let (status, statuses) = async_mpsc::unbounded_channel();
        let worker = Arc::clone(&recognizer);
        // Ends when the engine (and with it the job sender) is dropped.
        let spawned = std::thread::Builder::new()
            .name("j3-speech".into())
            .spawn(move || work(worker.as_ref(), &queue, &status));
        if let Err(e) = spawned {
            tracing::error!("starting the speech thread: {e}");
        }
        Arc::new(Self {
            source,
            recognizer,
            jobs,
            statuses: Mutex::new(Some(statuses)),
        })
    }

    /// This machine's microphone and whisper.cpp, with models kept in
    /// `models_dir` and downloaded on `runtime`.
    pub fn local(models_dir: PathBuf, runtime: tokio::runtime::Handle) -> Arc<Self> {
        Self::new(
            Box::new(microphone::Microphone),
            Arc::new(whisper::Whisper::new(models_dir, runtime)),
        )
    }

    pub fn hardware(&self) -> &Hardware {
        self.recognizer.hardware()
    }

    /// The model for this machine when the user has not picked one.
    pub fn recommended(&self) -> SpeechModel {
        models::recommend(self.hardware())
    }

    fn send(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            tracing::warn!("the speech thread is gone");
        }
    }
}

fn work(
    recognizer: &dyn Recognizer,
    queue: &mpsc::Receiver<Job>,
    status: &async_mpsc::UnboundedSender<ModelStatus>,
) {
    let prepare = |model: SpeechModel| {
        let result = recognizer.prepare(model, &mut |progress| {
            let _ = status.send(progress);
        });
        let _ = status.send(match &result {
            Ok(()) => ModelStatus::Ready(model),
            Err(e) => ModelStatus::Failed(e.clone()),
        });
        result
    };
    while let Ok(job) = queue.recv() {
        match job {
            Job::Prepare(model) => {
                if let Err(e) = prepare(model) {
                    tracing::warn!("speech model {}: {e}", model.key());
                }
            }
            Job::Transcribe {
                model,
                audio,
                pass,
                reply,
            } => {
                let result = prepare(model).and_then(|()| recognizer.transcribe(&audio, pass));
                let _ = reply.send(result);
            }
        }
    }
}

/// Where a capture is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Opening the microphone.
    Starting,
    Recording,
    /// The microphone is closed; the recording is being transcribed.
    Transcribing,
}

/// What the composer that started a capture hears about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechEvent {
    Phase {
        capture: String,
        phase: Phase,
    },
    /// The whole transcript so far; it replaces the previous one.
    Transcript {
        capture: String,
        text: String,
    },
    /// The capture is over: its final text, and why it failed if it did.
    End {
        capture: String,
        text: String,
        error: Option<String>,
    },
}

/// A recording turned into 16 kHz audio as it grows.
struct Converted {
    resampler: Resampler,
    audio: Vec<f32>,
}

impl Converted {
    /// Takes what the microphone added since the last call (all of it, the
    /// tail included, when `last`) and returns the whole recording so far.
    fn update(&mut self, incoming: &Mutex<Vec<f32>>, last: bool) -> Vec<f32> {
        let new = std::mem::take(
            &mut *incoming
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        self.resampler.push(&new, last, &mut self.audio);
        self.audio.clone()
    }
}

struct Capture {
    id: String,
    owner: EntityId,
    model: SpeechModel,
    phase: Phase,
    /// A stop was asked for.
    stopping: bool,
    /// What the microphone has recorded and not yet converted.
    incoming: Arc<Mutex<Vec<f32>>>,
    stream: Option<Box<dyn AudioStream>>,
    /// Set once the microphone is open (its rate decides the resampler).
    converted: Option<Arc<Mutex<Converted>>>,
    /// A partial transcription is running.
    partial: bool,
    /// The length of the audio the last partial transcribed.
    partial_len: usize,
    _ticks: Option<Task<()>>,
}

pub struct Speech {
    engine: Arc<SpeechEngine>,
    status: ModelStatus,
    capture: Option<Capture>,
    next_capture: u64,
    _statuses: Task<()>,
}

impl EventEmitter<SpeechEvent> for Speech {}

struct GlobalSpeech(Entity<Speech>);

impl Global for GlobalSpeech {}

impl Speech {
    /// The app's dictation, created on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(speech) = cx.try_global::<GlobalSpeech>() {
            return speech.0.clone();
        }
        let engine = Arc::clone(Services::global(cx).speech());
        let statuses = engine
            .statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let speech = cx.new(|cx: &mut Context<Self>| {
            let task = cx.spawn(async move |this, cx| {
                let Some(mut statuses) = statuses else {
                    return;
                };
                while let Some(status) = statuses.recv().await {
                    let updated = this.update(cx, |this, cx| {
                        this.status = status;
                        cx.notify();
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            });
            Self {
                engine,
                status: ModelStatus::Idle,
                capture: None,
                next_capture: 0,
                _statuses: task,
            }
        });
        cx.set_global(GlobalSpeech(speech.clone()));
        speech
    }

    /// The running capture's composer and phase.
    pub fn capture(&self) -> Option<(EntityId, Phase)> {
        self.capture.as_ref().map(|c| (c.owner, c.phase))
    }

    /// Where the speech model is.
    pub fn status(&self) -> &ModelStatus {
        &self.status
    }

    /// Starts dictating for the composer `owner` with `model` (the one
    /// recommended for this machine when `None`). Returns the capture's id,
    /// which its [`SpeechEvent`]s carry, or `None` while another capture
    /// runs (one microphone, one capture).
    pub fn start(
        &mut self,
        owner: EntityId,
        model: Option<SpeechModel>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if self.capture.is_some() {
            return None;
        }
        let model = model.unwrap_or_else(|| self.engine.recommended());
        self.next_capture += 1;
        let id = format!("j3-{}-{}", std::process::id(), self.next_capture);
        // The model gets ready while the user speaks.
        if self.status != ModelStatus::Ready(model) {
            self.engine.send(Job::Prepare(model));
        }
        let incoming = Arc::new(Mutex::new(Vec::new()));
        let engine = Arc::clone(&self.engine);
        let sink = Arc::clone(&incoming);
        let opening = cx.background_spawn(async move { engine.source.open(sink) });
        let opened = id.clone();
        cx.spawn(async move |this, cx| {
            let result = opening.await;
            let _ = this.update(cx, |this, cx| this.opened(&opened, result, cx));
        })
        .detach();
        self.capture = Some(Capture {
            id: id.clone(),
            owner,
            model,
            phase: Phase::Starting,
            stopping: false,
            incoming,
            stream: None,
            converted: None,
            partial: false,
            partial_len: 0,
            _ticks: None,
        });
        cx.notify();
        Some(id)
    }

    fn opened(
        &mut self,
        id: &str,
        result: Result<Box<dyn AudioStream>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(capture) = self.capture.as_mut().filter(|c| c.id == id) else {
            if let Ok(stream) = result {
                close(stream, cx);
            }
            return;
        };
        let stream = match result {
            Ok(stream) => stream,
            Err(e) => return self.end(id, String::new(), Some(e), cx),
        };
        if capture.stopping {
            // Cancelled before the microphone opened: nothing was said.
            close(stream, cx);
            return self.end(id, String::new(), None, cx);
        }
        capture.converted = Some(Arc::new(Mutex::new(Converted {
            resampler: Resampler::new(stream.rate()),
            audio: Vec::new(),
        })));
        capture.stream = Some(stream);
        capture.phase = Phase::Recording;
        let ticking = id.to_owned();
        capture._ticks = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.tick(&ticking, cx)).is_err() {
                    break;
                }
            }
        }));
        cx.emit(SpeechEvent::Phase {
            capture: id.to_owned(),
            phase: Phase::Recording,
        });
        cx.notify();
    }

    /// Transcribes the recording so far when the model is ready, no partial
    /// is running and there is enough new audio.
    fn tick(&mut self, id: &str, cx: &mut Context<Self>) {
        let status = &self.status;
        let Some(capture) = self.capture.as_mut().filter(|c| {
            c.id == id
                && c.phase == Phase::Recording
                && !c.partial
                && *status == ModelStatus::Ready(c.model)
        }) else {
            return;
        };
        let Some(converted) = capture.converted.clone() else {
            return;
        };
        capture.partial = true;
        let incoming = Arc::clone(&capture.incoming);
        let since = capture.partial_len;
        let model = capture.model;
        let engine = Arc::clone(&self.engine);
        let id = id.to_owned();
        cx.spawn(async move |this, cx| {
            let audio = cx
                .background_spawn(async move {
                    converted
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .update(&incoming, false)
                })
                .await;
            let len = audio.len();
            let result = if len >= since + PARTIAL_STEP {
                let (reply, answer) = oneshot::channel();
                engine.send(Job::Transcribe {
                    model,
                    audio,
                    pass: Pass::Partial,
                    reply,
                });
                answer.await.ok()
            } else {
                None
            };
            let _ = this.update(cx, |this, cx| {
                let Some(capture) = this.capture.as_mut().filter(|c| c.id == id) else {
                    return;
                };
                capture.partial = false;
                let Some(result) = result else {
                    return;
                };
                capture.partial_len = len;
                match result {
                    Ok(text) if capture.phase == Phase::Recording && !text.is_empty() => {
                        cx.emit(SpeechEvent::Transcript { capture: id, text });
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!("partial transcription: {e}"),
                }
            });
        })
        .detach();
    }

    /// Stops the running capture, cancelling it if the microphone is still
    /// opening; what was said is transcribed.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(capture) = self.capture.as_mut().filter(|c| !c.stopping) else {
            return;
        };
        capture.stopping = true;
        if capture.phase == Phase::Starting {
            // `opened` ends it when the microphone answers.
            return;
        }
        capture.phase = Phase::Transcribing;
        let id = capture.id.clone();
        cx.emit(SpeechEvent::Phase {
            capture: id.clone(),
            phase: Phase::Transcribing,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TAIL).await;
            let Ok(Some((stream, incoming, converted, model, engine))) =
                this.update(cx, |this, _| {
                    let capture = this.capture.as_mut().filter(|c| c.id == id)?;
                    Some((
                        capture.stream.take(),
                        Arc::clone(&capture.incoming),
                        capture.converted.clone()?,
                        capture.model,
                        Arc::clone(&this.engine),
                    ))
                })
            else {
                return;
            };
            let audio = cx
                .background_spawn(async move {
                    drop(stream);
                    converted
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .update(&incoming, true)
                })
                .await;
            let (reply, answer) = oneshot::channel();
            engine.send(Job::Transcribe {
                model,
                audio,
                pass: Pass::Final,
                reply,
            });
            let (text, error) = match answer.await {
                Ok(Ok(text)) => (text, None),
                Ok(Err(e)) => (String::new(), Some(e)),
                Err(_) => (String::new(), Some("the speech thread is gone".to_owned())),
            };
            let _ = this.update(cx, |this, cx| this.end(&id, text, error, cx));
        })
        .detach();
    }

    /// Ends the capture `id`, once.
    fn end(&mut self, id: &str, text: String, error: Option<String>, cx: &mut Context<Self>) {
        if self.capture.as_ref().is_none_or(|c| c.id != id) {
            return;
        }
        if let Some(stream) = self.capture.take().and_then(|c| c.stream) {
            close(stream, cx);
        }
        cx.emit(SpeechEvent::End {
            capture: id.to_owned(),
            text,
            error,
        });
        cx.notify();
    }
}

/// Stops a recording off the UI thread (closing a device can block).
fn close(stream: Box<dyn AudioStream>, cx: &mut App) {
    cx.background_spawn(async move { drop(stream) }).detach();
}
