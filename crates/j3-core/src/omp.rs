//! Driver for `omp --mode rpc` processes.
//!
//! One [`OmpSession`] is one omp process bound to one omp session (one j3
//! thread). The blocking [`omp_rpc::Client`] is bridged onto tokio: commands
//! run on the blocking pool and every unsolicited frame is forwarded to an
//! [`OmpEvents`] receiver. The receiver ends when the process exits or the
//! session is closed.

use std::{
    ffi::OsString,
    fs::File,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::Arc,
    thread,
};

pub use omp_rpc;
use omp_rpc::{
    Client, ClientOptions, Command, Event, ExtensionUiResponse, GetStateCommand, RpcInbound,
    SetAskDialogCommand,
};
use tokio::{sync::mpsc, task};

/// Frames from an omp process that are not responses to our own commands.
pub type OmpEvents = mpsc::UnboundedReceiver<Event>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Rpc(#[from] omp_rpc::Error),
    #[error("omp blocking task failed: {0}")]
    Join(#[from] task::JoinError),
    #[error("opening omp stderr log {path}: {source}")]
    StderrLog {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("omp reported no session file; is persistence disabled?")]
    NoSessionFile,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// How omp processes are launched. Shared by every thread of a server.
#[derive(Debug, Clone)]
pub struct OmpLauncher {
    program: PathBuf,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    stderr_log: Option<PathBuf>,
}

impl Default for OmpLauncher {
    fn default() -> Self {
        Self::new("omp")
    }
}

impl OmpLauncher {
    /// Launches `program` (an `omp` binary, resolved through `PATH` when bare).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            stderr_log: None,
        }
    }

    /// Extra argument passed after j3's own flags (e.g. `--model x`).
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Extra environment variable for every process.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Appends each process's stderr to `path` instead of inheriting it.
    pub fn stderr_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.stderr_log = Some(path.into());
        self
    }

    /// Starts omp in `cwd`. With `resume`, it reopens that session file;
    /// otherwise omp starts a fresh session in its default session directory,
    /// so `omp --resume` run in `cwd` lists it.
    pub async fn start(
        &self,
        cwd: &Path,
        resume: Option<&Path>,
    ) -> Result<(OmpSession, OmpEvents)> {
        let mode = match resume {
            Some(session_file) => Persistence::Resume(session_file),
            None => Persistence::Fresh,
        };
        self.spawn(cwd, mode).await
    }

    /// Starts an ephemeral omp (`--no-session`) in `cwd` for catalog queries
    /// (models, commands). Nothing is persisted, so `session_file()` is empty.
    pub async fn start_utility(&self, cwd: &Path) -> Result<(OmpSession, OmpEvents)> {
        self.spawn(cwd, Persistence::None).await
    }

    async fn spawn(&self, cwd: &Path, mode: Persistence<'_>) -> Result<(OmpSession, OmpEvents)> {
        let persistent = !matches!(mode, Persistence::None);
        let process = self.command(cwd, mode)?;
        let (client, frames) =
            task::spawn_blocking(move || Client::spawn(process, ClientOptions::default()))
                .await??;
        let client = Arc::new(client);

        let (tx, events) = mpsc::unbounded_channel();
        // The omp reader thread drops its sender when the process exits or the
        // client closes, which ends this loop and then `events`.
        thread::Builder::new()
            .name("omp-events".into())
            .spawn(move || {
                for frame in frames {
                    if tx.send(frame).is_err() {
                        break;
                    }
                }
            })
            .map_err(omp_rpc::Error::Io)?;

        let mut session = OmpSession {
            client: Some(client),
            session_id: String::new(),
            session_file: PathBuf::new(),
        };
        session.call(SetAskDialogCommand { enabled: true }).await?;
        let state = session.call(GetStateCommand {}).await?;
        session.session_id = state.session_id;
        if persistent {
            session.session_file = state
                .session_file
                .map(PathBuf::from)
                .ok_or(Error::NoSessionFile)?;
        }
        Ok((session, events))
    }

    fn command(&self, cwd: &Path, mode: Persistence<'_>) -> Result<Process> {
        let mut process = Process::new(&self.program);
        process
            .current_dir(cwd)
            .args(["--mode", "rpc", "--approval-mode=yolo"]);
        match mode {
            Persistence::Fresh => {}
            Persistence::Resume(session_file) => {
                process.arg("--session").arg(session_file);
            }
            Persistence::None => {
                process.arg("--no-session");
            }
        }
        process
            .args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        if let Some(path) = &self.stderr_log {
            let log = File::options()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|source| Error::StderrLog {
                    path: path.clone(),
                    source,
                })?;
            process.stderr(Stdio::from(log));
        }
        Ok(process)
    }
}

#[derive(Clone, Copy)]
enum Persistence<'a> {
    /// New session in omp's default session directory.
    Fresh,
    /// Reopen this session file.
    Resume(&'a Path),
    /// `--no-session`: nothing written.
    None,
}

/// A running omp process bound to one session.
pub struct OmpSession {
    /// `None` only while being dropped or closed.
    client: Option<Arc<Client>>,
    session_id: String,
    session_file: PathBuf,
}

impl OmpSession {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Session transcript; pass to [`OmpLauncher::start`] to resume.
    pub fn session_file(&self) -> &Path {
        &self.session_file
    }

    pub fn pid(&self) -> Option<u32> {
        self.client().pid()
    }

    fn client(&self) -> &Arc<Client> {
        self.client
            .as_ref()
            .expect("omp client present until close/drop")
    }

    /// Runs `command` and returns its decoded response. For `prompt`, the
    /// response is only the admission ack; the run's outcome arrives as
    /// `prompt_result` / `session_settled` events.
    pub async fn call<C>(&self, command: C) -> Result<C::Output>
    where
        C: Command + Send + 'static,
        C::Output: Send + 'static,
    {
        let client = Arc::clone(self.client());
        Ok(task::spawn_blocking(move || client.call(&command)).await??)
    }

    /// Answers an `extension_ui_request` (`ask`, `select`, `confirm`, ...).
    /// Returns once the frame is queued for omp.
    pub fn respond_ui(&self, response: ExtensionUiResponse) -> Result<()> {
        Ok(self
            .client()
            .send(&RpcInbound::ExtensionUiResponse(response))?)
    }

    /// Stops the process group and waits until it is gone. If a command is
    /// still in flight on another task, teardown happens when it finishes.
    pub async fn close(mut self) -> Result<()> {
        let client = self
            .client
            .take()
            .expect("omp client present until close/drop");
        task::spawn_blocking(move || match Arc::try_unwrap(client) {
            Ok(client) => client.close(),
            Err(_shared) => Ok(()),
        })
        .await??;
        Ok(())
    }
}

impl Drop for OmpSession {
    fn drop(&mut self) {
        // Client teardown blocks (SIGTERM, up to ~2s to SIGKILL and reap); keep
        // it off async worker threads.
        if let Some(client) = self.client.take() {
            let _ = thread::Builder::new()
                .name("omp-teardown".into())
                .spawn(move || drop(client));
        }
    }
}
