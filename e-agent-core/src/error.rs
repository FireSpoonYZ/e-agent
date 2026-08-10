#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("runtime error: {0:#}")]
    Py(#[from] pyo3::PyErr),
    #[error("openai error: {0}")]
    OpenAI(#[from] async_openai::error::OpenAIError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid string: {0}")]
    Nul(#[from] std::ffi::NulError),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
