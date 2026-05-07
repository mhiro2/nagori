use nagori_core::{AppError, Result};

pub fn unsupported<T>() -> Result<T> {
    Err(AppError::Unsupported(
        "Linux platform adapter is reserved for a later milestone".to_owned(),
    ))
}
