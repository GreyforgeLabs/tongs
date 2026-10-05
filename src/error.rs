//! Error type for store operations.

use std::fmt;
use std::io;
use std::path::Path;

use crate::python::path_repr;

/// The category of a store failure. These correspond to the Python 1.x
/// exception classes (`StoreError` and its subclasses, plus the `TypeError`,
/// `ValueError` and `OSError` cases the Python API raised).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Generic store failure (`StoreError`), e.g. trying to upgrade a held
    /// shared lock to exclusive.
    Store,
    /// The lock could not be acquired before the timeout (`LockTimeoutError`).
    LockTimeout,
    /// The file exists but is not a readable document (`CorruptStoreError`).
    Corrupt,
    /// The file's schema version cannot be reconciled (`SchemaVersionError`).
    SchemaVersion,
    /// An invalid constructor argument (Python `ValueError` / `TypeError`).
    InvalidArgument,
    /// `get()` / `set()` used on a document that is not a JSON object
    /// (Python `TypeError`).
    NotAMapping,
    /// A value could not be converted to JSON before writing (Python
    /// `TypeError` from the encoder). Nothing was written.
    Serialize,
    /// The stored document could not be converted to the requested type.
    Deserialize,
    /// A fallible migration returned an error.
    Migration,
    /// An operating-system error (Python `OSError`).
    Io,
}

/// An error from a tongs operation.
///
/// `Display` renders the same message the Python implementation put in its
/// exception, so CLI output stays identical.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

/// Result alias for store operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Create an error of the given kind with a message.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Attach an underlying cause.
    pub fn with_source(
        mut self,
        source: impl Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    ) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Wrap an I/O error, formatting it like CPython's `OSError.__str__`
    /// (`[Errno 13] Permission denied: 'path'`).
    pub fn io(err: io::Error, filename: Option<&Path>, filename2: Option<&Path>) -> Self {
        let message = match err.raw_os_error() {
            Some(code) => {
                let full = io::Error::from_raw_os_error(code).to_string();
                let suffix = format!(" (os error {code})");
                let strerror = full.strip_suffix(&suffix).unwrap_or(&full);
                let mut msg = format!("[Errno {code}] {strerror}");
                if let Some(f1) = filename {
                    msg.push_str(": ");
                    msg.push_str(&path_repr(f1));
                    if let Some(f2) = filename2 {
                        msg.push_str(" -> ");
                        msg.push_str(&path_repr(f2));
                    }
                }
                msg
            }
            None => err.to_string(),
        };
        Error {
            kind: ErrorKind::Io,
            message,
            source: Some(Box::new(err)),
        }
    }

    /// The error category.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The message (identical to `to_string()`).
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The underlying I/O error for [`ErrorKind::Io`] errors.
    pub fn io_error(&self) -> Option<&io::Error> {
        self.source.as_deref().and_then(|s| s.downcast_ref())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}
