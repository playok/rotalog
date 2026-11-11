use std::error::Error as StdError;
use std::fmt;
use std::io;

/// Errors produced while parsing the command line or while rotating logs.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Invalid command line or configuration.
    Usage(String),
    /// A runtime problem that is not directly an I/O error.
    Runtime(String),
    /// An I/O operation failed, with a human-readable context string.
    Io { context: String, source: io::Error },
}

impl Error {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::Runtime(message.into())
    }

    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Exit status: 1 for bad usage, 2 for runtime failures.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 1,
            Self::Runtime(_) | Self::Io { .. } => 2,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Runtime(message) => f.write_str(message),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Attach context to an [`io::Result`] without pulling in a large error crate.
pub trait IoContext<T> {
    fn context(self, context: impl Into<String>) -> Result<T>;

    fn with_context<F>(self, context: F) -> Result<T>
    where
        F: FnOnce() -> String;
}

impl<T> IoContext<T> for io::Result<T> {
    fn context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::io(context, source))
    }

    fn with_context<F>(self, context: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.map_err(|source| Error::io(context(), source))
    }
}
