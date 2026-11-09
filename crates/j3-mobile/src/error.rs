use j3_client::envelope::ErrorCode;

/// Why a command failed. Swift sees it as a thrown `ClientError`.
#[derive(Debug, Clone, PartialEq, thiserror::Error, uniffi::Error)]
pub enum ClientError {
    /// The server answered with an error (wire.md §3.1). `message` is
    /// written for the user when `code` is `rejected`.
    #[error("{message}")]
    Server {
        code: ErrorCode,
        message: String,
        /// The error's `data` as JSON text, when it has any.
        data_json: Option<String>,
    },
    /// No live connection: not connected yet, lost while the call was open,
    /// or `Host::close` was called.
    #[error("not connected to the server")]
    Disconnected,
    /// An HTTP request (upload) failed.
    #[error("{message}")]
    Http { message: String },
    /// A response did not have the expected shape.
    #[error("unexpected response from the server: {message}")]
    Decode { message: String },
}

impl From<j3_client::Error> for ClientError {
    fn from(error: j3_client::Error) -> Self {
        match error {
            j3_client::Error::Server(error) => Self::Server {
                code: error.code,
                message: error.message,
                data_json: error.data.map(|data| data.to_string()),
            },
            j3_client::Error::Disconnected => Self::Disconnected,
            j3_client::Error::Http(message) => Self::Http { message },
            j3_client::Error::Decode(message) => Self::Decode { message },
        }
    }
}
