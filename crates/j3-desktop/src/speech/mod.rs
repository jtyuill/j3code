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
//! worker thread that owns the recognizer. [`SpeechUi`] is its face in the
//! app state (`J3::speech`): it holds the one capture that may run at a time
//! and reports its progress as [`SpeechEvent`]s to the composer that started
//! it, through `J3::on_speech_event`.
//!
//! How a composer dictates:
//! - The mic button reads [`SpeechUi::capture`]: a capture owned by another
//!   composer disables it, one owned by this composer gives its phase
//!   (spinner while [`Phase::Starting`], stop while [`Phase::Recording`],
//!   busy while [`Phase::Transcribing`]); [`SpeechUi::model_note`] adds the
//!   model's download or load progress to the tooltip.
//! - Clicking it calls `J3::start_dictation` with no capture running, or
//!   `J3::stop_dictation` while this composer's capture is starting or
//!   recording. A composer that goes away mid-capture calls
//!   `J3::cancel_dictation`.
//! - `J3::on_speech_event` gets every [`SpeechEvent`] of the capture, tagged
//!   with its owner: the phase once the microphone is open and again when
//!   it stops, the growing transcript while recording, and exactly one
//!   [`SpeechEvent::End`] with the final text (and the error, if any).

mod microphone;
pub mod models;
mod resample;
mod whisper;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use tokio::sync::{mpsc as async_mpsc, oneshot};

use self::{
    models::{Hardware, SpeechModel},
    resample::{Resampler, WHISPER_RATE},
};
use crate::{app::J3, composer::ComposerKey};

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
    /// Taken once, by the first dictation, to keep [`SpeechUi`]'s model
    /// status current.
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

/// What the composer that started a capture hears about it, through
/// `J3::on_speech_event`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechEvent {
    Phase {
        owner: ComposerKey,
        phase: Phase,
    },
    /// The whole transcript so far; it replaces the previous one.
    Transcript {
        owner: ComposerKey,
        text: String,
    },
    /// The capture is over: its final text, and why it failed if it did.
    End {
        owner: ComposerKey,
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

/// Converts what `converted` has not seen of `incoming` off the UI thread
/// (dropping `stream` first, which stops the recording, when given) and
/// returns the whole recording so far.
async fn convert(
    converted: Arc<Mutex<Converted>>,
    incoming: Arc<Mutex<Vec<f32>>>,
    stream: Option<Box<dyn AudioStream>>,
    last: bool,
) -> Result<Vec<f32>, String> {
    tokio::task::spawn_blocking(move || {
        drop(stream);
        converted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update(&incoming, last)
    })
    .await
    .map_err(|e| format!("converting the recording: {e}"))
}

struct Capture {
    /// Tells this capture's answers from those of an earlier one that
    /// arrive late.
    id: u64,
    owner: ComposerKey,
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
    /// When (`Clock::t`) to next check for a partial pass, while recording.
    tick_at: Option<f64>,
    /// When (`Clock::t`) the tail after a stop is over and the final pass
    /// starts.
    tail_at: Option<f64>,
}

/// The app's dictation: the speech model's status and the one capture that
/// may run at a time.
pub struct SpeechUi {
    status: ModelStatus,
    capture: Option<Capture>,
    next_capture: u64,
    /// The engine's model statuses are forwarded into `status`.
    listening: bool,
}

impl Default for SpeechUi {
    fn default() -> Self {
        Self {
            status: ModelStatus::Idle,
            capture: None,
            next_capture: 0,
            listening: false,
        }
    }
}

impl SpeechUi {
    /// The running capture's composer and phase.
    pub fn capture(&self) -> Option<(&ComposerKey, Phase)> {
        self.capture.as_ref().map(|c| (&c.owner, c.phase))
    }

    /// Where the speech model is.
    pub fn status(&self) -> &ModelStatus {
        &self.status
    }

    /// What the speech model is busy with, for the dictation button's
    /// tooltip: `downloading the Small speech model (40%)` or `loading the
    /// Small speech model`; `None` when it is not busy.
    pub fn model_note(&self) -> Option<String> {
        match &self.status {
            ModelStatus::Downloading { model, fraction } => Some(format!(
                "downloading the {} speech model ({:.0}%)",
                model.label(),
                fraction * 100.
            )),
            ModelStatus::Loading(model) => {
                Some(format!("loading the {} speech model", model.label()))
            }
            ModelStatus::Idle | ModelStatus::Ready(_) | ModelStatus::Failed(_) => None,
        }
    }
}

impl J3 {
    /// Starts dictating for the composer `owner` with the model picked in
    /// the settings (the one recommended for this machine when none is).
    /// Does nothing while another capture runs (one microphone, one
    /// capture): a composer checks [`SpeechUi::capture`] first, and after
    /// the call finds itself the owner in [`Phase::Starting`].
    pub fn start_dictation(&mut self, owner: ComposerKey, _tx: &mut rune::Tx) {
        if self.speech.capture.is_some() {
            return;
        }
        self.forward_model_statuses();
        let engine = Arc::clone(&self.services.speech);
        let model = self
            .prefs
            .ui()
            .dictation_model()
            .unwrap_or_else(|| engine.recommended());
        self.speech.next_capture += 1;
        let id = self.speech.next_capture;
        // The model gets ready while the user speaks.
        if self.speech.status != ModelStatus::Ready(model) {
            engine.send(Job::Prepare(model));
        }
        let incoming = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&incoming);
        self.spawn_blocking(move || {
            let result = engine.source.open(sink);
            move |s: &mut J3, tx: &mut rune::Tx| s.dictation_opened(id, result, tx)
        });
        self.speech.capture = Some(Capture {
            id,
            owner,
            model,
            phase: Phase::Starting,
            stopping: false,
            incoming,
            stream: None,
            converted: None,
            partial: false,
            partial_len: 0,
            tick_at: None,
            tail_at: None,
        });
    }

    /// Stops the running capture, cancelling it if the microphone is still
    /// opening; what was said is transcribed and arrives as its
    /// [`SpeechEvent::End`].
    pub fn stop_dictation(&mut self, tx: &mut rune::Tx) {
        let Some(capture) = self.speech.capture.as_mut().filter(|c| !c.stopping) else {
            return;
        };
        capture.stopping = true;
        if capture.phase == Phase::Starting {
            // `dictation_opened` ends it when the microphone answers.
            return;
        }
        capture.phase = Phase::Transcribing;
        capture.tick_at = None;
        capture.tail_at = Some(self.clock.t + TAIL.as_secs_f64());
        tx.wake_after(TAIL);
        let owner = capture.owner.clone();
        self.on_speech_event(
            SpeechEvent::Phase {
                owner,
                phase: Phase::Transcribing,
            },
            tx,
        );
    }

    /// Ends the running capture at once, whatever its phase, discarding
    /// what was recorded: its [`SpeechEvent::End`] carries no text.
    pub fn cancel_dictation(&mut self, tx: &mut rune::Tx) {
        if let Some(id) = self.speech.capture.as_ref().map(|c| c.id) {
            self.end_dictation(id, String::new(), None, tx);
        }
    }

    /// Runs the running capture's due deadlines: a partial pass every
    /// [`TICK`] while recording, the final pass once the [`TAIL`] after a
    /// stop is over. True when one was due.
    pub(crate) fn poll_speech(&mut self, tx: &mut rune::Tx) -> bool {
        let now = self.clock.t;
        let Some(capture) = self.speech.capture.as_mut() else {
            return false;
        };
        let id = capture.id;
        if capture.tail_at.is_some_and(|at| now >= at) {
            capture.tail_at = None;
            self.transcribe_recording(id);
            return true;
        }
        if capture.tick_at.is_some_and(|at| now >= at) {
            capture.tick_at = Some(now + TICK.as_secs_f64());
            tx.wake_after(TICK);
            self.transcribe_partial(id);
            return true;
        }
        false
    }

    /// Keeps [`SpeechUi::status`] current from the first dictation on.
    fn forward_model_statuses(&mut self) {
        if self.speech.listening {
            return;
        }
        self.speech.listening = true;
        let statuses = self
            .services
            .speech
            .statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(mut statuses) = statuses else {
            return;
        };
        let inbox = self.inbox.clone();
        self.services.runtime.spawn(async move {
            while let Some(status) = statuses.recv().await {
                inbox.post(move |s, _| s.speech.status = status);
            }
        });
    }

    fn dictation_opened(
        &mut self,
        id: u64,
        result: Result<Box<dyn AudioStream>, String>,
        tx: &mut rune::Tx,
    ) {
        let Some(capture) = self.speech.capture.as_mut().filter(|c| c.id == id) else {
            if let Ok(stream) = result {
                close(&self.services.runtime, stream);
            }
            return;
        };
        let stream = match result {
            Ok(stream) => stream,
            Err(e) => return self.end_dictation(id, String::new(), Some(e), tx),
        };
        if capture.stopping {
            // Cancelled before the microphone opened: nothing was said.
            close(&self.services.runtime, stream);
            return self.end_dictation(id, String::new(), None, tx);
        }
        capture.converted = Some(Arc::new(Mutex::new(Converted {
            resampler: Resampler::new(stream.rate()),
            audio: Vec::new(),
        })));
        capture.stream = Some(stream);
        capture.phase = Phase::Recording;
        capture.tick_at = Some(self.clock.t + TICK.as_secs_f64());
        tx.wake_after(TICK);
        let owner = capture.owner.clone();
        self.on_speech_event(
            SpeechEvent::Phase {
                owner,
                phase: Phase::Recording,
            },
            tx,
        );
    }

    /// Transcribes the recording so far when the model is ready, no partial
    /// is running and there is enough new audio.
    fn transcribe_partial(&mut self, id: u64) {
        let status = &self.speech.status;
        let Some(capture) = self.speech.capture.as_mut().filter(|c| {
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
        let engine = Arc::clone(&self.services.speech);
        self.spawn(async move {
            let audio = convert(converted, incoming, None, false)
                .await
                .unwrap_or_default();
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
            move |s: &mut J3, tx: &mut rune::Tx| s.dictation_partial(id, len, result, tx)
        });
    }

    fn dictation_partial(
        &mut self,
        id: u64,
        len: usize,
        result: Option<Result<String, String>>,
        tx: &mut rune::Tx,
    ) {
        let Some(capture) = self.speech.capture.as_mut().filter(|c| c.id == id) else {
            return;
        };
        capture.partial = false;
        let Some(result) = result else {
            return;
        };
        capture.partial_len = len;
        match result {
            Ok(text) if capture.phase == Phase::Recording && !text.is_empty() => {
                let owner = capture.owner.clone();
                self.on_speech_event(SpeechEvent::Transcript { owner, text }, tx);
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("partial transcription: {e}"),
        }
    }

    /// Closes the microphone after the tail and transcribes the whole
    /// recording, ending the capture with the result.
    fn transcribe_recording(&mut self, id: u64) {
        let Some(capture) = self.speech.capture.as_mut().filter(|c| c.id == id) else {
            return;
        };
        let Some(converted) = capture.converted.clone() else {
            return;
        };
        let stream = capture.stream.take();
        let incoming = Arc::clone(&capture.incoming);
        let model = capture.model;
        let engine = Arc::clone(&self.services.speech);
        self.spawn(async move {
            let (text, error) = match convert(converted, incoming, stream, true).await {
                Ok(audio) => {
                    let (reply, answer) = oneshot::channel();
                    engine.send(Job::Transcribe {
                        model,
                        audio,
                        pass: Pass::Final,
                        reply,
                    });
                    match answer.await {
                        Ok(Ok(text)) => (text, None),
                        Ok(Err(e)) => (String::new(), Some(e)),
                        Err(_) => (String::new(), Some("the speech thread is gone".to_owned())),
                    }
                }
                Err(e) => (String::new(), Some(e)),
            };
            move |s: &mut J3, tx: &mut rune::Tx| s.end_dictation(id, text, error, tx)
        });
    }

    /// Ends the capture `id`, once.
    fn end_dictation(&mut self, id: u64, text: String, error: Option<String>, tx: &mut rune::Tx) {
        let Some(capture) = self.speech.capture.take_if(|c| c.id == id) else {
            return;
        };
        if let Some(stream) = capture.stream {
            close(&self.services.runtime, stream);
        }
        self.on_speech_event(
            SpeechEvent::End {
                owner: capture.owner,
                text,
                error,
            },
            tx,
        );
    }
}

/// Stops a recording off the UI thread (closing a device can block).
fn close(runtime: &tokio::runtime::Handle, stream: Box<dyn AudioStream>) {
    runtime.spawn_blocking(move || drop(stream));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_note_tells_a_download_and_a_load_only() {
        let mut speech = SpeechUi::default();
        assert_eq!(speech.model_note(), None);
        speech.status = ModelStatus::Downloading {
            model: SpeechModel::SmallEn,
            fraction: 0.4,
        };
        assert_eq!(
            speech.model_note().as_deref(),
            Some("downloading the Small speech model (40%)")
        );
        speech.status = ModelStatus::Loading(SpeechModel::SmallEn);
        assert_eq!(
            speech.model_note().as_deref(),
            Some("loading the Small speech model")
        );
        speech.status = ModelStatus::Ready(SpeechModel::SmallEn);
        assert_eq!(speech.model_note(), None);
        assert!(speech.capture().is_none());
    }
}
