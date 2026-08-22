//! Thin wrapper around external command execution with logging and dry-run.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::{Error, Result};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(v: bool) {
    VERBOSE.store(v, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Return true if `tool` resolves on PATH.
pub fn have(tool: &str) -> bool {
    // Search PATH manually to avoid spawning `which`.
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let candidate = std::path::Path::new(dir).join(tool);
            if candidate.is_file() {
                return true;
            }
        }
    }
    // Common sbin locations that may be missing from a reduced PATH.
    for dir in ["/sbin", "/usr/sbin", "/usr/local/sbin", "/bin", "/usr/bin"] {
        if std::path::Path::new(dir).join(tool).is_file() {
            return true;
        }
    }
    false
}

/// Ensure every named tool exists, otherwise return `MissingTool`.
pub fn require(tools: &[&str]) -> Result<()> {
    for t in tools {
        if !have(t) {
            return Err(Error::MissingTool {
                tool: (*t).to_string(),
            });
        }
    }
    Ok(())
}

pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == Some(0)
    }
}

/// Run a command, capturing output. Errors only on spawn failure.
pub fn run_raw(args: &[&str], stdin: Option<&str>) -> Result<Output> {
    run_owned(
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        stdin,
    )
}

pub fn run_owned(args: &[String], stdin: Option<&str>) -> Result<Output> {
    if args.is_empty() {
        return Err(Error::Execution("empty command".into()));
    }
    if is_verbose() {
        eprintln!("+ {}", args.join(" "));
    }
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..]);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    use std::io::Write;
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::MissingTool {
                tool: args[0].clone(),
            })
        }
        Err(e) => return Err(Error::Io(e.to_string())),
    };
    if let Some(data) = stdin {
        if let Some(mut sink) = child.stdin.take() {
            sink.write_all(data.as_bytes())?;
        }
    } else {
        drop(child.stdin.take());
    }
    let out = child.wait_with_output()?;
    Ok(Output {
        status: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Run a command and fail with a descriptive error if it exits non-zero.
pub fn run_checked(args: &[&str], stdin: Option<&str>) -> Result<Output> {
    let out = run_raw(args, stdin)?;
    if !out.ok() {
        return Err(Error::Command {
            program: args.join(" "),
            code: out.status,
            stderr: if out.stderr.trim().is_empty() {
                out.stdout.clone()
            } else {
                out.stderr.clone()
            },
        });
    }
    Ok(out)
}

/// Capture stdout of a checked command as a trimmed string.
pub fn capture(args: &[&str]) -> Result<String> {
    Ok(run_checked(args, None)?.stdout)
}
