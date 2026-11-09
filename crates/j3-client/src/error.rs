use j3_protocol::v2::envelope::{self, ErrorCode};

/// Why a v2 call failed. `Display` is a human-readable sentence.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    /// The server answered the call (or the connection, `err` with `id:
    /// null`) with an error (wire.md §3.1). `message` is written for the
    /// user when `code` is `rejected`.
    #[error("{}", .0.message)]
    Server(envelope::Error),
    /// No live connection: not connected yet, lost while the call was open,
    /// or [`crate::Connection::close`] was called.
    #[error("not connected to the server")]
    Disconnected,
    /// An HTTP request (upload) failed.
    #[error("{0}")]
    Http(String),
    /// A frame or response did not have the expected shape.
    #[error("unexpected response from the server: {0}")]
    Decode(String),
}

impl Error {
    /// The server's error code, for [`Error::Server`].
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Server(error) => Some(error.code),
            _ => None,
        }
    }
}
