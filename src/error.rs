//! Error type shared by every module. Each variant maps to one exit code.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Bad command-line input or path selection.
    #[error("{0}")]
    Usage(String),
    /// Invalid or unsupported configuration or repository content.
    #[error("{0}")]
    Config(String),
    /// Work blocked by a conflict with existing destination state.
    #[error("{0}")]
    Conflict(String),
    /// Filesystem error with the operation and path that failed.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    /// Any other runtime failure (render, decryption, subprocess).
    #[error("{0}")]
    Failed(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Process exit code for this error. See docs/cli.md.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) | Error::Config(_) => 2,
            Error::Conflict(_) => 3,
            Error::Io { .. } | Error::Failed(_) => 4,
        }
    }

    /// True for a filesystem error that says the path does not exist.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}

/// Adds a description of the failed operation to an `io::Error`.
pub trait IoContext<T> {
    fn ctx(self, context: impl FnOnce() -> String) -> Result<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn ctx(self, context: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|source| Error::Io {
            context: context(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_documented_table() {
        assert_eq!(Error::Usage("x".into()).exit_code(), 2);
        assert_eq!(Error::Config("x".into()).exit_code(), 2);
        assert_eq!(Error::Conflict("x".into()).exit_code(), 3);
        let io = Err::<(), _>(io::Error::other("boom")).ctx(|| "read /x".into());
        let io = io.unwrap_err();
        assert_eq!(io.to_string(), "read /x: boom");
        assert_eq!(io.exit_code(), 4);
        assert_eq!(Error::Failed("x".into()).exit_code(), 4);
    }
}
