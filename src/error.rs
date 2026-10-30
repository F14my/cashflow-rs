use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl Error {
    pub fn new(msg: impl Into<String>) -> Self {
        Error(msg.into())
    }

    pub fn context(self, ctx: impl fmt::Display) -> Self {
        Error(format!("{ctx}: {}", self.0))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! bail {
    ($($arg:tt)*) => { return Err($crate::error::Error::new(format!($($arg)*))) };
}
pub(crate) use bail;
