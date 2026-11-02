//! Terminal sessions (t3's `TerminalManager`,
//! `apps/server/src/terminal/Manager.ts`): one login shell per (thread,
//! terminal id) in a PTY on this host, so a thread's terminal runs where its
//! agent does. Output is kept in a capped history; `terminal.attach` replays
//! it as a `snapshot` and then streams live events to every attached client.
//! Sessions outlive their clients and end on `terminal.close`, when their
//! thread is deleted, or when the server shuts down.
//!
//! Each shell's PTY is read on its own OS thread, never on the runtime.

use std::{
    borrow::Cow,
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use futures_util::stream::{self, BoxStream, StreamExt};
use j3_protocol::t3::{
    now_iso,
    terminal::{
        MAX_COLS, MAX_ROWS, TerminalAttachInput, TerminalAttachSnapshot, TerminalAttachStreamEvent,
        TerminalCloseInput, TerminalEnv, TerminalError, TerminalErrorEvent, TerminalEventBase,
        TerminalExited, TerminalNotice, TerminalOpenInput, TerminalOutput, TerminalResizeInput,
        TerminalRestarted, TerminalSessionSnapshot, TerminalSessionStatus, TerminalWriteInput,
    },
};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

/// Most history a session keeps (t3's `DEFAULT_MAX_TERMINAL_BUFFER_BYTES`).
pub const MAX_HISTORY_BYTES: usize = 512 * 1024;
/// Size of a shell started without one.
const DEFAULT_SIZE: PtySize = PtySize {
    rows: 24,
    cols: 80,
    pixel_width: 0,
    pixel_height: 0,
};
/// One PTY read.
const READ_CHUNK: usize = 16 * 1024;

type Key = (String, String);
type Subscriber = mpsc::UnboundedSender<TerminalAttachStreamEvent>;

/// Every terminal session of the server.
#[derive(Default)]
pub struct Terminals {
    sessions: Mutex<HashMap<Key, Arc<Session>>>,
}

struct Session {
    thread_id: String,
    terminal_id: String,
    state: Mutex<State>,
}

struct State {
    cwd: String,
    status: TerminalSessionStatus,
    pid: Option<u32>,
    history: History,
    exit_code: Option<i64>,
    exit_signal: Option<i64>,
    updated_at: String,
    size: PtySize,
    process: Option<Process>,
    /// Bumped for every spawned shell, so a replaced shell's late output
    /// and exit are ignored.
    generation: u64,
    subscribers: Vec<Subscriber>,
}

/// A running shell. Dropping it hangs the shell up.
struct Process {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// `None` once the shell has exited, so its reaped pid is never signalled.
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut killer) = self.killer.take() {
            let _ = killer.kill();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// t3's `getTerminalLabel`: `term-3` is "Terminal 3".
fn label(terminal_id: &str) -> String {
    match terminal_id.strip_prefix("term-") {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            format!("Terminal {n}")
        }
        _ => terminal_id.chars().take(128).collect(),
    }
}

/// `cols`/`rows` clamped to the contract's ranges.
fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(1, MAX_ROWS),
        cols: cols.clamp(1, MAX_COLS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The size an attach or open asks for: both `cols` and `rows`, or none.
fn requested_size(cols: Option<u16>, rows: Option<u16>) -> Option<PtySize> {
    Some(pty_size(cols?, rows?))
}

/// The user's login shell.
fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

fn check_cwd(cwd: &str) -> Result<(), TerminalError> {
    match std::fs::metadata(Path::new(cwd)) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(TerminalError::cwd_not_directory(cwd)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(TerminalError::cwd_not_found(cwd))
        }
        Err(error) => Err(TerminalError::cwd_stat(cwd, error.to_string())),
    }
}

impl Session {
    fn base(&self) -> TerminalEventBase {
        TerminalEventBase {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            sequence: None,
        }
    }

    fn snapshot(&self, state: &State) -> TerminalSessionSnapshot {
        TerminalSessionSnapshot {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: state.cwd.clone(),
            worktree_path: None,
            status: state.status,
            pid: state.pid,
            history: state.history.text.clone(),
            exit_code: state.exit_code,
            exit_signal: state.exit_signal,
            label: label(&self.terminal_id),
            updated_at: state.updated_at.clone(),
            sequence: None,
        }
    }

    /// Starts a shell in `state.cwd`. On failure the session is left in
    /// `error` with the reason returned.
    fn spawn(self: &Arc<Self>, state: &mut State, env: Option<&TerminalEnv>) -> Result<(), String> {
        let result = self.try_spawn(state, env);
        state.updated_at = now_iso();
        if let Err(message) = &result {
            state.status = TerminalSessionStatus::Error;
            state.pid = None;
            tracing::warn!(
                thread = %self.thread_id, terminal = %self.terminal_id, %message,
                "starting terminal"
            );
        }
        result
    }

    fn try_spawn(
        self: &Arc<Self>,
        state: &mut State,
        env: Option<&TerminalEnv>,
    ) -> Result<(), String> {
        let pty = native_pty_system()
            .openpty(state.size)
            .map_err(|error| format!("opening a terminal: {error}"))?;
        let mut command = CommandBuilder::new(shell());
        command.arg("-l");
        command.cwd(&state.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (key, value) in env.into_iter().flatten() {
            command.env(key, value);
        }
        let child = pty
            .slave
            .spawn_command(command)
            .map_err(|error| format!("starting {}: {error}", shell()))?;
        // The shell holds the only slave descriptor, so its exit ends reads.
        drop(pty.slave);
        let reader = pty
            .master
            .try_clone_reader()
            .map_err(|error| format!("reading the terminal: {error}"))?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|error| format!("writing the terminal: {error}"))?;
        state.generation += 1;
        state.process = Some(Process {
            master: pty.master,
            writer: Arc::new(Mutex::new(writer)),
            killer: Some(child.clone_killer()),
        });
        state.status = TerminalSessionStatus::Running;
        state.pid = child.process_id();
        state.exit_code = None;
        state.exit_signal = None;
        let session = Arc::clone(self);
        let generation = state.generation;
        std::thread::Builder::new()
            .name(format!("j3-terminal-{}", self.terminal_id))
            .spawn(move || session.pump(generation, reader, child))
            .map(drop)
            .map_err(|error| {
                state.process = None;
                format!("starting the terminal reader: {error}")
            })
    }

    /// The reader thread: forwards output until the shell's side closes,
    /// then reports how it exited.
    fn pump(
        &self,
        generation: u64,
        mut reader: Box<dyn Read + Send>,
        mut child: Box<dyn Child + Send + Sync>,
    ) {
        let mut buffer = vec![0; READ_CHUNK];
        let mut decoder = Utf8Decoder::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => self.output(generation, decoder.decode(&buffer[..read])),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        self.output(generation, decoder.finish());
        let (exit_code, exit_signal) = wait(child.as_mut());
        let mut state = lock(&self.state);
        if state.generation != generation {
            return;
        }
        if let Some(mut process) = state.process.take() {
            process.killer = None;
        }
        state.status = TerminalSessionStatus::Exited;
        state.pid = None;
        state.exit_code = exit_code;
        state.exit_signal = exit_signal;
        state.updated_at = now_iso();
        let event = TerminalAttachStreamEvent::Exited(TerminalExited {
            base: self.base(),
            exit_code,
            exit_signal,
        });
        broadcast(&mut state.subscribers, event);
    }

    fn output(&self, generation: u64, data: String) {
        if data.is_empty() {
            return;
        }
        let mut state = lock(&self.state);
        if state.generation != generation {
            return;
        }
        state.history.push(&data);
        let event = TerminalAttachStreamEvent::Output(TerminalOutput {
            base: self.base(),
            data,
        });
        broadcast(&mut state.subscribers, event);
    }

    /// Hangs the shell up and ends every attached stream with `closed`.
    fn close(&self) {
        let mut state = lock(&self.state);
        state.generation += 1;
        state.process = None;
        let event = TerminalAttachStreamEvent::Closed(TerminalNotice { base: self.base() });
        broadcast(&mut state.subscribers, event);
        state.subscribers.clear();
    }

    fn resize(state: &mut State, size: PtySize) -> Result<(), String> {
        if state.size.cols == size.cols && state.size.rows == size.rows {
            return Ok(());
        }
        if let Some(process) = &state.process {
            process
                .master
                .resize(size)
                .map_err(|error| error.to_string())?;
        }
        state.size = size;
        state.updated_at = now_iso();
        Ok(())
    }
}

/// Sends `event` to every subscriber, dropping detached ones.
fn broadcast(subscribers: &mut Vec<Subscriber>, event: TerminalAttachStreamEvent) {
    subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

/// Waits for the shell and returns its exit code or terminating signal.
fn wait(child: &mut dyn Child) -> (Option<i64>, Option<i64>) {
    #[cfg(unix)]
    if let Some(child) = child.downcast_mut::<std::process::Child>() {
        use std::os::unix::process::ExitStatusExt;
        return match child.wait() {
            Ok(status) => (status.code().map(i64::from), status.signal().map(i64::from)),
            Err(_) => (None, None),
        };
    }
    match child.wait() {
        Ok(status) => (Some(i64::from(status.exit_code())), None),
        Err(_) => (None, None),
    }
}

/// When a session's shell is started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Start {
    /// Only when the session does not exist yet.
    IfMissing,
    /// Also when its shell exited or failed to start.
    IfNotRunning,
}

impl Terminals {
    pub fn new() -> Self {
        Self::default()
    }

    /// The session for the key, starting its shell in `cwd` when it does
    /// not exist or (with [`Start::IfNotRunning`]) is not running; a
    /// running one is resized to `size`. Returns the session and why its
    /// shell failed to start, if it did. Starts happen under the sessions
    /// lock, so one key never gets two shells.
    fn ensure(
        &self,
        thread_id: &str,
        terminal_id: &str,
        cwd: &str,
        size: Option<PtySize>,
        env: Option<&TerminalEnv>,
        start: Start,
    ) -> Result<(Arc<Session>, Option<String>), TerminalError> {
        let mut sessions = lock(&self.sessions);
        let key = (thread_id.to_owned(), terminal_id.to_owned());
        if let Some(session) = sessions.get(&key) {
            let session = Arc::clone(session);
            drop(sessions);
            let mut state = lock(&session.state);
            if start == Start::IfMissing || state.process.is_some() {
                if let Some(size) = size {
                    let _ = Session::resize(&mut state, size);
                }
                drop(state);
                return Ok((session, None));
            }
            check_cwd(cwd)?;
            state.cwd = cwd.to_owned();
            if let Some(size) = size {
                state.size = size;
            }
            let failure = session.spawn(&mut state, env).err();
            let event = TerminalAttachStreamEvent::Restarted(TerminalRestarted {
                base: session.base(),
                snapshot: session.snapshot(&state),
            });
            broadcast(&mut state.subscribers, event);
            drop(state);
            return Ok((session, failure));
        }
        check_cwd(cwd)?;
        let session = Arc::new(Session {
            thread_id: thread_id.to_owned(),
            terminal_id: terminal_id.to_owned(),
            state: Mutex::new(State {
                cwd: cwd.to_owned(),
                status: TerminalSessionStatus::Starting,
                pid: None,
                history: History::new(MAX_HISTORY_BYTES),
                exit_code: None,
                exit_signal: None,
                updated_at: now_iso(),
                size: size.unwrap_or(DEFAULT_SIZE),
                process: None,
                generation: 0,
                subscribers: Vec::new(),
            }),
        });
        let failure = session.spawn(&mut lock(&session.state), env).err();
        sessions.insert(key, Arc::clone(&session));
        Ok((session, failure))
    }

    /// `terminal.attach`: a `snapshot` of the session, then its live
    /// events. A missing session starts in `input.cwd`, else in
    /// `default_cwd`; an exited one restarts when `restartIfNotRunning`.
    /// The stream ends after `closed`.
    pub fn attach(
        &self,
        input: &TerminalAttachInput,
        default_cwd: &str,
    ) -> Result<BoxStream<'static, TerminalAttachStreamEvent>, TerminalError> {
        let cwd = input.cwd.as_deref().unwrap_or(default_cwd);
        let start = if input.restart_if_not_running == Some(true) {
            Start::IfNotRunning
        } else {
            Start::IfMissing
        };
        let (session, failure) = self.ensure(
            &input.thread_id,
            &input.terminal_id,
            cwd,
            requested_size(input.cols, input.rows),
            input.env.as_ref(),
            start,
        )?;
        let (subscriber, events) = mpsc::unbounded_channel();
        let mut first = vec![];
        {
            let mut state = lock(&session.state);
            first.push(TerminalAttachStreamEvent::Snapshot(
                TerminalAttachSnapshot {
                    snapshot: session.snapshot(&state),
                },
            ));
            if let Some(message) = failure {
                first.push(TerminalAttachStreamEvent::Error(TerminalErrorEvent {
                    base: session.base(),
                    message,
                }));
            }
            state.subscribers.push(subscriber);
        }
        let live = stream::unfold(events, |mut events| async move {
            let event = events.recv().await?;
            Some((event, events))
        });
        Ok(stream::iter(first).chain(live).boxed())
    }

    /// `terminal.open`: starts the session (or restarts it when it is not
    /// running) and returns its snapshot.
    pub fn open(
        &self,
        input: &TerminalOpenInput,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        let (session, _) = self.ensure(
            &input.thread_id,
            &input.terminal_id,
            &input.cwd,
            requested_size(input.cols, input.rows),
            input.env.as_ref(),
            Start::IfNotRunning,
        )?;
        let state = lock(&session.state);
        Ok(session.snapshot(&state))
    }

    fn session(&self, thread_id: &str, terminal_id: &str) -> Result<Arc<Session>, TerminalError> {
        lock(&self.sessions)
            .get(&(thread_id.to_owned(), terminal_id.to_owned()))
            .cloned()
            .ok_or_else(|| TerminalError::session_lookup(thread_id, terminal_id))
    }

    /// `terminal.write`: sends `data` to the shell, off the runtime (a full
    /// PTY blocks the write).
    pub async fn write(&self, input: TerminalWriteInput) -> Result<(), TerminalError> {
        let session = self.session(&input.thread_id, &input.terminal_id)?;
        let (writer, pid) = {
            let state = lock(&session.state);
            match &state.process {
                Some(process) => (Arc::clone(&process.writer), state.pid.unwrap_or_default()),
                None => {
                    return Err(TerminalError::not_running(
                        &input.thread_id,
                        &input.terminal_id,
                    ));
                }
            }
        };
        let data = input.data;
        let written = tokio::task::spawn_blocking(move || {
            let mut writer = lock(&writer);
            writer.write_all(data.as_bytes())?;
            writer.flush()
        })
        .await;
        match written {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(TerminalError::write(
                input.thread_id,
                input.terminal_id,
                pid,
                error.to_string(),
            )),
            Err(error) => Err(TerminalError::write(
                input.thread_id,
                input.terminal_id,
                pid,
                error.to_string(),
            )),
        }
    }

    /// `terminal.resize`.
    pub fn resize(&self, input: &TerminalResizeInput) -> Result<(), TerminalError> {
        let session = self.session(&input.thread_id, &input.terminal_id)?;
        let mut state = lock(&session.state);
        if state.process.is_none() {
            return Err(TerminalError::not_running(
                &input.thread_id,
                &input.terminal_id,
            ));
        }
        let size = pty_size(input.cols, input.rows);
        Session::resize(&mut state, size).map_err(|cause| {
            TerminalError::resize(
                &input.thread_id,
                &input.terminal_id,
                state.pid.unwrap_or_default(),
                (size.cols, size.rows),
                cause,
            )
        })
    }

    /// `terminal.close`: one terminal, or every terminal of the thread
    /// without a `terminalId`. Closing an unknown terminal is not an error.
    pub fn close(&self, input: &TerminalCloseInput) {
        match &input.terminal_id {
            Some(terminal_id) => {
                let key = (input.thread_id.clone(), terminal_id.clone());
                let removed = lock(&self.sessions).remove(&key);
                if let Some(session) = removed {
                    session.close();
                }
            }
            None => self.close_thread(&input.thread_id),
        }
    }

    /// Closes every terminal of a thread (it was deleted).
    pub fn close_thread(&self, thread_id: &str) {
        let removed: Vec<Arc<Session>> = {
            let mut sessions = lock(&self.sessions);
            let keys: Vec<Key> = sessions
                .keys()
                .filter(|(thread, _)| thread == thread_id)
                .cloned()
                .collect();
            keys.iter().filter_map(|key| sessions.remove(key)).collect()
        };
        for session in removed {
            session.close();
        }
    }

    /// Closes every terminal (server shutdown).
    pub fn shutdown(&self) {
        let removed: Vec<Arc<Session>> = lock(&self.sessions).drain().map(|(_, s)| s).collect();
        for session in removed {
            session.close();
        }
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Shell output kept for `snapshot`s, at most `max` bytes. Past that the
/// oldest output is dropped down to 7/8 of `max` (cut after a newline when
/// one is near), so trimming stays rare.
struct History {
    text: String,
    max: usize,
}

impl History {
    fn new(max: usize) -> Self {
        Self {
            text: String::new(),
            max,
        }
    }

    fn push(&mut self, data: &str) {
        self.text.push_str(data);
        if self.text.len() <= self.max {
            return;
        }
        let slack = self.max / 8;
        let mut cut = self.text.len() - (self.max - slack);
        while !self.text.is_char_boundary(cut) {
            cut += 1;
        }
        if let Some(newline) = self.text.as_bytes()[cut..]
            .iter()
            .take(slack)
            .position(|&byte| byte == b'\n')
        {
            cut += newline + 1;
        }
        self.text.drain(..cut);
    }
}

/// Decodes PTY output, carrying a UTF-8 sequence split across reads to the
/// next one. Invalid bytes become U+FFFD.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn decode(&mut self, bytes: &[u8]) -> String {
        let data: Cow<'_, [u8]> = if self.pending.is_empty() {
            Cow::Borrowed(bytes)
        } else {
            let mut joined = std::mem::take(&mut self.pending);
            joined.extend_from_slice(bytes);
            Cow::Owned(joined)
        };
        let mut text = String::with_capacity(data.len());
        let mut rest: &[u8] = &data;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    return text;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).expect("validated prefix"));
                    match error.error_len() {
                        Some(invalid) => {
                            text.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[invalid..];
                        }
                        None => {
                            self.pending.extend_from_slice(after);
                            return text;
                        }
                    }
                }
            }
        }
    }

    /// What is left at the end of the output: a truncated sequence.
    fn finish(&mut self) -> String {
        if std::mem::take(&mut self.pending).is_empty() {
            String::new()
        } else {
            char::REPLACEMENT_CHARACTER.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_sequences_split_across_reads_are_carried() {
        let text = "héllo → 🦀";
        let bytes = text.as_bytes();
        // Split at every byte offset, including inside multi-byte sequences.
        for split in 0..=bytes.len() {
            let mut decoder = Utf8Decoder::default();
            let mut out = decoder.decode(&bytes[..split]);
            out += &decoder.decode(&bytes[split..]);
            out += &decoder.finish();
            assert_eq!(out, text, "split at {split}");
        }
        let mut decoder = Utf8Decoder::default();
        let crab = "🦀".as_bytes();
        let mut out = String::new();
        for byte in crab {
            out += &decoder.decode(std::slice::from_ref(byte));
        }
        assert_eq!(out, "🦀");
    }

    #[test]
    fn invalid_and_truncated_bytes_become_replacement_characters() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.decode(b"a\xffb"), "a\u{FFFD}b");
        assert_eq!(decoder.decode(b"c\xe2\x86"), "c");
        assert_eq!(decoder.finish(), "\u{FFFD}");
        assert_eq!(decoder.decode(b"d"), "d");
    }

    #[test]
    fn history_is_capped_keeping_the_newest_output() {
        let mut history = History::new(1024);
        for n in 0..500 {
            history.push(&format!("line {n} é\n"));
            assert!(history.text.len() <= 1024);
        }
        assert!(history.text.ends_with("line 499 é\n"));
        // Trimmed after a newline, so the replay starts on a whole line.
        assert!(history.text.starts_with("line "));
        assert!(history.text.len() >= 1024 * 7 / 8 - 16);

        // Without newlines it still cuts on a character boundary.
        let mut history = History::new(64);
        for _ in 0..100 {
            history.push("ééé");
        }
        assert!(history.text.len() <= 64);
        assert!(history.text.chars().all(|c| c == 'é'));
    }

    #[test]
    fn labels_follow_t3() {
        assert_eq!(label("term-1"), "Terminal 1");
        assert_eq!(label("term-12"), "Terminal 12");
        assert_eq!(label("scratch"), "scratch");
    }
}
