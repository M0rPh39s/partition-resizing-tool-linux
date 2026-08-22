//! Error type with administrator-friendly messages.
//!
//! Errors are meant to be read by a Linux administrator, so each variant
//! renders a clear explanation and, where useful, a suggested remedy — never a
//! bare "exit code 1".

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// A required external utility is not installed.
    MissingTool { tool: String },
    /// A command exited non-zero; carries a human explanation.
    Command {
        program: String,
        code: Option<i32>,
        stderr: String,
    },
    /// The storage topology could not be discovered.
    Discovery(String),
    /// The requested target is ambiguous or invalid.
    InvalidTarget(String),
    /// The operation is not supported for this filesystem/stack.
    Unsupported(String),
    /// The operation was refused for safety reasons.
    Safety(String),
    /// The operation requires the filesystem to be offline; suggests a remedy.
    RequiresOffline {
        mountpoint: String,
        fstype: String,
        suggestion: String,
    },
    /// A plan could not be constructed (e.g. insufficient space).
    Plan(String),
    /// Something failed while executing a validated plan.
    Execution(String),
    /// Persistent state is missing or inconsistent.
    State(String),
    /// Wrapping for std::io errors.
    Io(String),
    /// JSON (de)serialisation error.
    Json(String),
    /// The user declined a confirmation prompt.
    Aborted,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MissingTool { tool } => write!(
                f,
                "required utility '{tool}' is not installed or not on PATH"
            ),
            Error::Command {
                program,
                code,
                stderr,
            } => {
                let code = code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into());
                write!(
                    f,
                    "'{program}' failed (exit {code})\n    {}",
                    stderr.trim().replace('\n', "\n    ")
                )
            }
            Error::Discovery(m) => write!(f, "could not analyse storage: {m}"),
            Error::InvalidTarget(m) => write!(f, "invalid target: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported operation: {m}"),
            Error::Safety(m) => write!(f, "refused for safety: {m}"),
            Error::RequiresOffline {
                mountpoint,
                fstype,
                suggestion,
            } => write!(
                f,
                "cannot perform this operation while {mountpoint} is mounted.\n\
                 Filesystem: {fstype}\n\
                 {fstype} shrinking requires the filesystem to be offline.\n\
                 Suggested solution:\n    {suggestion}"
            ),
            Error::Plan(m) => write!(f, "cannot build a safe plan: {m}"),
            Error::Execution(m) => write!(f, "operation failed: {m}"),
            Error::State(m) => write!(f, "operation state problem: {m}"),
            Error::Io(m) => write!(f, "I/O error: {m}"),
            Error::Json(m) => write!(f, "data format error: {m}"),
            Error::Aborted => write!(f, "aborted by user"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e.to_string())
    }
}
