use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum TMDLError {
    #[error("Unknown error")]
    Unknown,
    #[error("File error: {0}")]
    IO(String),
    #[error("Unexpected expression")]
    UnexpectedExpression,
}

impl From<std::io::Error> for TMDLError {
    fn from(value: std::io::Error) -> Self {
        Self::IO(format!("{:?}", value))
    }
}
