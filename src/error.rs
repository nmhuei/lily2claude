use thiserror::Error;

#[derive(Error, Debug)]
pub enum LilyError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Upstream HTTP error: {0}")]
    UpstreamHttp(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("ACP agent error: {0}")]
    Acp(String),
    #[error("Rate limited (429)")]
    RateLimited,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, LilyError>;
