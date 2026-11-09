//! Terminals (wire.md §5.11): one login shell per (thread, terminal id) in a
//! PTY on this host, started in the thread's project directory, so a
//! thread's terminal runs where its agent does. Output is kept in a capped
//! history; `terminal.attach` replays it as a `snapshot` and then streams
//! live items to every attached client. Sessions outlive their clients and
//! end on `terminal.close`, when their thread is deleted, or when the server
//! shuts down.
//!
//! Each shell's PTY is read on its own OS thread, never on the runtime.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use futures_util::stream::{self, BoxStream, StreamExt};
use j3_protocol::v2::envelope::{Error, ErrorCode};
use j3_protocol::v2::methods::{
    AttachParams, TerminalCloseParams, TerminalItem, TerminalResizeParams, TerminalSnapshot,
    TerminalState, TerminalWriteParams,
};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

/// Most history a session keeps.
pub const MAX_HISTORY_BYTES: usize = 512 * 1024;
/// Widest terminal a client may ask for.
const MAX_COLS: u16 = 1000;
/// Tallest terminal a client may ask for.
const MAX_ROWS: u16 = 500;
/// Most characters one `terminal.write` sends.
const MAX_WRITE_CHARS: usize = 65_536;
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
type Subscriber = mpsc::UnboundedSender<TerminalItem>;

/// Every terminal session of the server.
#[derive(Default)]
pub struct Terminals {
    sessions: Mutex<HashMap<Key, Arc<Session>>>,
}

struct Session {
    terminal_id: String,
    state: Mutex<State>,
}

struct State {
    cwd: String,
    status: TerminalState,
    history: History,
    exit_code: Option<i64>,
    exit_signal: Option<i64>,
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

fn unavailable(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Unavailable, message)
}

/// `cols`/`rows` as a PTY size; `bad_request` outside 1–1000 × 1–500.
fn pty_size(cols: u16, rows: u16) -> Result<PtySize, Error> {
    if !(1..=MAX_COLS).contains(&cols) || !(1..=MAX_ROWS).contains(&rows) {
        return Err(Error::bad_request(format!(
            "A terminal is 1–{MAX_COLS} columns by 1–{MAX_ROWS} rows, not {cols}×{rows}."
        )));
    }
    Ok(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

/// The user's login shell.
fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

fn check_cwd(cwd: &str) -> Result<(), Error> {
    match std::fs::metadata(Path::new(cwd)) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(unavailable(format!("{cwd} is not a directory."))),
        Err(error) => Err(unavailable(format!("{cwd}: {error}"))),
    }
}

impl Session {
    fn snapshot(state: &State) -> TerminalSnapshot {
        TerminalSnapshot {
            history: state.history.text.clone(),
            state: state.status,
            exit_code: state.exit_code,
            exit_signal: state.exit_signal,
        }
    }

    /// Starts a shell in `state.cwd`. On failure the session is left
    /// `failed` with the reason returned.
    fn spawn(
        self: &Arc<Self>,
        state: &mut State,
        env: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let result = self.try_spawn(state, env);
        if let Err(message) = &result {
            state.status = TerminalState::Failed;
            tracing::warn!(terminal = %self.terminal_id, %message, "starting terminal");
        }
        result
    }

    fn try_spawn(
        self: &Arc<Self>,
        state: &mut State,
        env: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let pty = native_pty_system()
            .openpty(state.size)
            .map_err(|error| format!("opening a terminal: {error}"))?;
        let mut command = CommandBuilder::new(shell());
        command.arg("-l");
        command.cwd(&state.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (key, value) in env {
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
        state.status = TerminalState::Running;
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
        state.status = TerminalState::Exited;
        state.exit_code = exit_code;
        state.exit_signal = exit_signal;
        let item = TerminalItem::Exited {
            exit_code,
            exit_signal,
        };
        broadcast(&mut state.subscribers, item);
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
        broadcast(&mut state.subscribers, TerminalItem::Output { data });
    }

    /// Hangs the shell up and ends every attached stream with `closed`.
    fn close(&self) {
        let mut state = lock(&self.state);
        state.generation += 1;
        state.process = None;
        broadcast(&mut state.subscribers, TerminalItem::Closed);
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
        Ok(())
    }
}

/// Sends `item` to every subscriber, dropping detached ones.
fn broadcast(subscribers: &mut Vec<Subscriber>, item: TerminalItem) {
    subscribers.retain(|subscriber| subscriber.send(item.clone()).is_ok());
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

impl Terminals {
    pub fn new() -> Self {
        Self::default()
    }

    /// The session for the key, starting its shell in `cwd` when it does
    /// not exist or (with `restart`) is not running; a running one is
    /// resized to `size`. Returns the session and why its shell failed to
    /// start, if it did. Starts happen under the sessions lock, so one key
    /// never gets two shells.
    fn ensure(
        &self,
        params: &AttachParams,
        cwd: &str,
        size: Option<PtySize>,
    ) -> Result<(Arc<Session>, Option<String>), Error> {
        let mut sessions = lock(&self.sessions);
        let key = (params.thread_id.clone(), params.terminal_id.clone());
        if let Some(session) = sessions.get(&key) {
            let session = Arc::clone(session);
            drop(sessions);
            let mut state = lock(&session.state);
            if params.restart != Some(true) || state.process.is_some() {
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
            let failure = session.spawn(&mut state, &params.env).err();
            let item = TerminalItem::Restarted(Session::snapshot(&state));
            broadcast(&mut state.subscribers, item);
            if let Some(message) = &failure {
                let message = message.clone();
                broadcast(&mut state.subscribers, TerminalItem::Failed { message });
                state.subscribers.clear();
            }
            drop(state);
            return Ok((session, failure));
        }
        check_cwd(cwd)?;
        let session = Arc::new(Session {
            terminal_id: params.terminal_id.clone(),
            state: Mutex::new(State {
                cwd: cwd.to_owned(),
                status: TerminalState::Running,
                history: History::new(MAX_HISTORY_BYTES),
                exit_code: None,
                exit_signal: None,
                size: size.unwrap_or(DEFAULT_SIZE),
                process: None,
                generation: 0,
                subscribers: Vec::new(),
            }),
        });
        let failure = session.spawn(&mut lock(&session.state), &params.env).err();
        sessions.insert(key, Arc::clone(&session));
        Ok((session, failure))
    }

    /// `terminal.attach`: a `snapshot` of the session, then its live items;
    /// the stream ends after `closed` or `failed`. A missing session starts
    /// its shell in `cwd` (the project directory); an exited one restarts
    /// when `params.restart`. `cols` and `rows` size the shell only when
    /// both are given.
    pub fn attach(
        &self,
        params: &AttachParams,
        cwd: &str,
    ) -> Result<BoxStream<'static, TerminalItem>, Error> {
        let size = match (params.cols, params.rows) {
            (Some(cols), Some(rows)) => Some(pty_size(cols, rows)?),
            _ => None,
        };
        let (session, failure) = self.ensure(params, cwd, size)?;
        let mut state = lock(&session.state);
        let snapshot = TerminalItem::Snapshot(Session::snapshot(&state));
        if let Some(message) = failure {
            let first = [snapshot, TerminalItem::Failed { message }];
            return Ok(stream::iter(first).boxed());
        }
        let (subscriber, items) = mpsc::unbounded_channel();
        state.subscribers.push(subscriber);
        drop(state);
        let live = stream::unfold(Some(items), |items| async move {
            let mut items = items?;
            let item = items.recv().await?;
            let last = matches!(item, TerminalItem::Closed | TerminalItem::Failed { .. });
            Some((item, (!last).then_some(items)))
        });
        Ok(stream::once(async { snapshot }).chain(live).boxed())
    }

    fn session(&self, thread_id: &str, terminal_id: &str) -> Result<Arc<Session>, Error> {
        lock(&self.sessions)
            .get(&(thread_id.to_owned(), terminal_id.to_owned()))
            .cloned()
            .ok_or_else(|| Error::not_found("terminal", terminal_id))
    }

    /// `terminal.write`: sends `data` (1–65,536 characters) to the shell,
    /// off the runtime (a full PTY blocks the write).
    pub async fn write(&self, params: TerminalWriteParams) -> Result<(), Error> {
        let chars = params.data.chars().count();
        if !(1..=MAX_WRITE_CHARS).contains(&chars) {
            return Err(Error::bad_request(format!(
                "Terminal input is 1–{MAX_WRITE_CHARS} characters, not {chars}."
            )));
        }
        let session = self.session(&params.thread_id, &params.terminal_id)?;
        let writer = match &lock(&session.state).process {
            Some(process) => Arc::clone(&process.writer),
            None => return Err(unavailable("The terminal's shell is not running.")),
        };
        let data = params.data;
        let written = tokio::task::spawn_blocking(move || {
            let mut writer = lock(&writer);
            writer.write_all(data.as_bytes())?;
            writer.flush()
        })
        .await;
        match written {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(unavailable(format!("writing the terminal: {error}"))),
            Err(error) => Err(unavailable(format!("writing the terminal: {error}"))),
        }
    }

    /// `terminal.resize`.
    pub fn resize(&self, params: &TerminalResizeParams) -> Result<(), Error> {
        let size = pty_size(params.cols, params.rows)?;
        let session = self.session(&params.thread_id, &params.terminal_id)?;
        let mut state = lock(&session.state);
        if state.process.is_none() {
            return Err(unavailable("The terminal's shell is not running."));
        }
        Session::resize(&mut state, size)
            .map_err(|cause| unavailable(format!("resizing the terminal: {cause}")))
    }

    /// `terminal.close`: one terminal, or every terminal of the thread
    /// without a `terminalId`. Closing an unknown terminal succeeds.
    pub fn close(&self, params: &TerminalCloseParams) {
        match &params.terminal_id {
            Some(terminal_id) => {
                let key = (params.thread_id.clone(), terminal_id.clone());
                let removed = lock(&self.sessions).remove(&key);
                if let Some(session) = removed {
                    session.close();
                }
            }
            None => self.close_thread(&params.thread_id),
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

    fn attach_params(restart: bool) -> AttachParams {
        AttachParams {
            thread_id: "t1".into(),
            terminal_id: "term-1".into(),
            cols: Some(80),
            rows: Some(24),
            env: BTreeMap::from([("J3_MARK".to_owned(), "marked".to_owned())]),
            restart: Some(restart),
        }
    }

    fn write_params(data: &str) -> TerminalWriteParams {
        TerminalWriteParams {
            thread_id: "t1".into(),
            terminal_id: "term-1".into(),
            data: data.into(),
        }
    }

    /// Items until one matches, with the output seen on the way.
    async fn until(
        items: &mut BoxStream<'static, TerminalItem>,
        done: impl Fn(&TerminalItem, &str) -> bool,
    ) -> TerminalItem {
        let mut output = String::new();
        let wait = async {
            loop {
                let item = items.next().await.expect("stream ended");
                if let TerminalItem::Output { data } = &item {
                    output.push_str(data);
                }
                if done(&item, &output) {
                    return item;
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), wait)
            .await
            .expect("terminal item timed out")
    }

    #[tokio::test]
    async fn attach_write_resize_exit_restart_close() {
        let terminals = Terminals::new();
        let cwd = std::env::temp_dir();
        let cwd = cwd.to_str().unwrap();

        let mut items = terminals.attach(&attach_params(false), cwd).unwrap();
        let Some(TerminalItem::Snapshot(first)) = items.next().await else {
            panic!("snapshot first");
        };
        assert_eq!(first.state, TerminalState::Running);

        terminals
            .write(write_params("echo $J3_MARK-$((40+2))\n"))
            .await
            .unwrap();
        until(&mut items, |_, output| output.contains("marked-42")).await;

        let resize = |cols, rows| TerminalResizeParams {
            thread_id: "t1".into(),
            terminal_id: "term-1".into(),
            cols,
            rows,
        };
        terminals.resize(&resize(100, 30)).unwrap();
        terminals.write(write_params("stty size\n")).await.unwrap();
        until(&mut items, |_, output| output.contains("30 100")).await;
        assert_eq!(
            terminals.resize(&resize(0, 30)).unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            terminals.resize(&resize(80, 501)).unwrap_err().code,
            ErrorCode::BadRequest
        );
        let unknown = TerminalResizeParams {
            terminal_id: "term-9".into(),
            ..resize(80, 24)
        };
        assert_eq!(
            terminals.resize(&unknown).unwrap_err().code,
            ErrorCode::NotFound
        );
        let empty = terminals.write(write_params("")).await.unwrap_err();
        assert_eq!(empty.code, ErrorCode::BadRequest);
        let huge = "x".repeat(MAX_WRITE_CHARS + 1);
        assert_eq!(
            terminals.write(write_params(&huge)).await.unwrap_err().code,
            ErrorCode::BadRequest
        );

        terminals.write(write_params("exit 3\n")).await.unwrap();
        let exited = until(&mut items, |item, _| {
            matches!(item, TerminalItem::Exited { .. })
        })
        .await;
        assert_eq!(
            exited,
            TerminalItem::Exited {
                exit_code: Some(3),
                exit_signal: None
            }
        );
        assert_eq!(
            terminals.write(write_params("x")).await.unwrap_err().code,
            ErrorCode::Unavailable
        );

        // A second client gets the history replayed.
        let mut replay = terminals.attach(&attach_params(false), cwd).unwrap();
        let Some(TerminalItem::Snapshot(snapshot)) = replay.next().await else {
            panic!("snapshot first");
        };
        assert_eq!(snapshot.state, TerminalState::Exited);
        assert_eq!(snapshot.exit_code, Some(3));
        assert!(snapshot.history.contains("marked-42"));

        // Restarting keeps the history and tells attached clients.
        let mut restarted = terminals.attach(&attach_params(true), cwd).unwrap();
        let Some(TerminalItem::Snapshot(snapshot)) = restarted.next().await else {
            panic!("snapshot first");
        };
        assert_eq!(snapshot.state, TerminalState::Running);
        let Some(TerminalItem::Restarted(snapshot)) = items.next().await else {
            panic!("restarted");
        };
        assert_eq!(snapshot.state, TerminalState::Running);
        assert!(snapshot.history.contains("marked-42"));
        terminals
            .write(write_params("echo again-$((1+1))\n"))
            .await
            .unwrap();
        until(&mut restarted, |_, output| output.contains("again-2")).await;

        terminals.close(&TerminalCloseParams {
            thread_id: "t1".into(),
            terminal_id: None,
            delete_history: None,
        });
        for mut stream in [items, replay, restarted] {
            until(&mut stream, |item, _| *item == TerminalItem::Closed).await;
            assert!(stream.next().await.is_none(), "stream ends after closed");
        }
        assert_eq!(
            terminals.write(write_params("x")).await.unwrap_err().code,
            ErrorCode::NotFound
        );
        // Closing an unknown terminal succeeds.
        terminals.close(&TerminalCloseParams {
            thread_id: "t1".into(),
            terminal_id: Some("term-9".into()),
            delete_history: None,
        });
    }

    #[test]
    fn attach_refuses_bad_sizes_and_missing_folders() {
        let terminals = Terminals::new();
        let mut params = attach_params(false);
        params.cols = Some(1001);
        let cwd = std::env::temp_dir();
        let error = terminals
            .attach(&params, cwd.to_str().unwrap())
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::BadRequest);
        let error = terminals
            .attach(&attach_params(false), "/nonexistent/j3-terminal")
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::Unavailable);
    }
}
