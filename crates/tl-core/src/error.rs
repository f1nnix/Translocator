use std::io;

/// Errors every provider maps its failures into, so the UI can react the
/// same way regardless of where a file lives (e.g. offer sudo on
/// `PermissionDenied`).
#[derive(Debug, Clone, thiserror::Error)]
pub enum VfsError {
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("No such file or directory: {0}")]
    NotFound(String),
    #[error("Already exists: {0}")]
    AlreadyExists(String),
    #[error("Cancelled")]
    Cancelled,
    #[error("Not supported: {0}")]
    Unsupported(String),
    #[error("Connection error: {0}")]
    Connection(String),
    #[error("Authentication failed: {0}")]
    Auth(String),
    #[error("{0}")]
    Other(String),
}

pub type VfsResult<T> = Result<T, VfsError>;

impl VfsError {
    pub fn other(msg: impl Into<String>) -> Self {
        VfsError::Other(msg.into())
    }

    /// Wraps an I/O error with the path it happened on.
    pub fn io(err: io::Error, context: impl std::fmt::Display) -> Self {
        let msg = format!("{context}: {err}");
        match err.kind() {
            io::ErrorKind::PermissionDenied => VfsError::PermissionDenied(msg),
            io::ErrorKind::NotFound => VfsError::NotFound(msg),
            io::ErrorKind::AlreadyExists => VfsError::AlreadyExists(msg),
            _ if err.raw_os_error() == Some(libc::EPERM) => VfsError::PermissionDenied(msg),
            _ => VfsError::Other(msg),
        }
    }

    pub fn is_permission_denied(&self) -> bool {
        matches!(self, VfsError::PermissionDenied(_))
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, VfsError::Cancelled)
    }

    /// Classifies a command-line tool's error output (`cp: cannot stat ...:
    /// No such file or directory`) into the closest variant.
    pub fn from_tool_output(stderr: &str) -> Self {
        let msg = stderr.trim().to_string();
        let lower = msg.to_ascii_lowercase();
        if lower.contains("permission denied") || lower.contains("operation not permitted") {
            VfsError::PermissionDenied(msg)
        } else if lower.contains("no such file or directory") {
            VfsError::NotFound(msg)
        } else if lower.contains("file exists") {
            VfsError::AlreadyExists(msg)
        } else {
            VfsError::Other(msg)
        }
    }
}

impl From<io::Error> for VfsError {
    fn from(err: io::Error) -> Self {
        let msg = err.to_string();
        match err.kind() {
            io::ErrorKind::PermissionDenied => VfsError::PermissionDenied(msg),
            io::ErrorKind::NotFound => VfsError::NotFound(msg),
            io::ErrorKind::AlreadyExists => VfsError::AlreadyExists(msg),
            _ => VfsError::Other(msg),
        }
    }
}
