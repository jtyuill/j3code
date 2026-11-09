//! j3 protocol client (`docs/protocol/wire.md`): one [`Connection`] per j3
//! server, plus the UI-agnostic thread-list workflows shared by the desktop
//! and the iOS app ([`inbox`], [`herdr`]) and the git control's choices
//! ([`git`]).
//!
//! Executor-agnostic: sockets and HTTP run on the tokio runtime handed to
//! [`Connection::open`], every public `async fn` can be awaited from any
//! executor, and state is published as `Arc` snapshots through
//! [`tokio::sync::watch`].

mod connection;
mod error;
pub mod git;
pub mod herdr;
pub mod inbox;
mod rpc;
mod state;
mod supervisor;

pub use connection::{Connection, GitRun, TerminalSession, ThreadWatch};
pub use error::Error;
/// The wire types (`j3_protocol::v2`).
pub use j3_protocol::v2::{PROTOCOL, entities, envelope, methods, streams};
pub use state::{ConnectionState, Info, Shell, Status, ThreadSync, ThreadView};

/// A thread with the key of the environment (connection) it lives in.
/// Thread ids are unique only within an environment.
#[derive(Debug, Clone, Copy)]
pub struct EnvThread<'a> {
    pub environment: &'a str,
    pub thread: &'a entities::Thread,
}

/// The title a new thread gets from its first prompt (`threads.create`'s
/// `title`): the prompt's words on one line, cut to 72 characters with
/// "...", or "Attached image(s)" / "New thread" for a prompt without text.
pub fn prompt_title(text: &str, images: usize) -> String {
    const MAX: usize = 72;
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return match images {
            0 => "New thread",
            1 => "Attached image",
            _ => "Attached images",
        }
        .to_owned();
    }
    if title.chars().count() <= MAX {
        return title;
    }
    let head: String = title.chars().take(MAX - 3).collect();
    format!("{}...", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::prompt_title;

    #[test]
    fn titles_come_from_the_prompt() {
        assert_eq!(prompt_title("  fix\n the   feed ", 0), "fix the feed");
        assert_eq!(prompt_title("", 0), "New thread");
        assert_eq!(prompt_title(" ", 1), "Attached image");
        assert_eq!(prompt_title("", 3), "Attached images");
        let long = "word ".repeat(30);
        let title = prompt_title(&long, 0);
        assert!(title.ends_with("...") && title.chars().count() <= 72);
    }
}
