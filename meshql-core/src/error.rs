#[derive(thiserror::Error, Debug)]
pub enum MeshqlError {
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Template error: {0}")]
    Template(String),
    #[error("Parse error: {0}")]
    Parse(String),
    /// The adapter does not implement this capability. Returned rather than a
    /// silent empty result, so a caller can tell "nothing to report" from
    /// "this store cannot answer".
    #[error("Unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, MeshqlError>;
