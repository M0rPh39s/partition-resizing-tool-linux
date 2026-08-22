//! Small shared utilities: command execution and size handling.

pub mod cmd;
pub mod sizes;

use crate::errors::Result;

/// Create a unique temporary directory under `/tmp` and return its path.
pub fn tempdir(prefix: &str) -> Result<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let path = format!("/tmp/{prefix}-{pid}-{nanos}");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
