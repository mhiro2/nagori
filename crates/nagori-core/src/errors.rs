use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("search error: {0}")]
    Search(String),
    #[error("platform error: {0}")]
    Platform(String),
    #[error("permission error: {0}")]
    Permission(String),
    #[error("ai error: {0}")]
    Ai(String),
    #[error("policy error: {0}")]
    Policy(String),
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("configuration error: {0}")]
    Configuration(String),
}
