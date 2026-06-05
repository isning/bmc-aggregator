use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("device not connected")]
    NotConnected,

    #[error("encoder error: {0}")]
    Encoder(#[from] crate::bmc::encoder::error::EncoderError),

    #[error("timeout")]
    Timeout,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl From<String> for SessionError {
    fn from(s: String) -> Self {
        SessionError::Other(s)
    }
}
