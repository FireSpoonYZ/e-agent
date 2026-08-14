pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Extension error: {0}")]
    Extension(String),
    #[error("IO error: {0}")]
    Io(#[from] Box<std::io::Error>),
    #[error("JSON error: {0}")]
    Json(#[from] Box<serde_json::Error>),
}

impl Error {
    pub fn extension(message: impl Into<String>) -> Self {
        Self::Extension(message.into())
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(Box::new(error))
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(Box::new(error))
    }
}
