use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum TMDLError {
    #[error("Unknown error")]
    Unknown,
    #[error("File error: {0}")]
    IO(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Unexpected expression")]
    UnexpectedExpression,
}

impl From<std::io::Error> for TMDLError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(format!("{:?}", value))
    }
}

impl From<serde_json::Error> for TMDLError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}
