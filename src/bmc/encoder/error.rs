use thiserror::Error;

#[derive(Error, Debug)]
pub enum EncoderError {
    #[error("encoder initialization failed: {0}")]
    InitFailed(String),

    #[error("encode failed: {0}")]
    EncodeFailed(String),

    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),

    #[error("unsupported pixel format: {0}")]
    UnsupportedFormat(String),

    #[error("NVENC error: {operation} (status={status})")]
    Nvenc {
        operation: &'static str,
        status: u32,
    },
}
