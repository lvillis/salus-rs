use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Failure(String),
    #[error("{0}")]
    InvalidConfig(String),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn failure(message: impl Into<String>) -> Self {
        Self::Failure(message.into())
    }

    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self::InvalidConfig(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Failure(_) => 1,
            Self::InvalidConfig(_) => 3,
            Self::Internal(_) => 4,
        }
    }

    pub fn print_and_exit_code_with_quiet(&self, quiet: bool) -> i32 {
        if !quiet {
            eprintln!("{self}");
        }
        self.exit_code()
    }
}
