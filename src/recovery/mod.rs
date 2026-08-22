//! Persistent recovery state machine + backups.
//!
//! State lives in `/var/lib/partresize/state.json` (overridable via
//! `PARTRESIZE_STATE_DIR`) so a resize spanning a reboot — or interrupted by
//! power loss — can be detected and recovered on the next boot.

use serde::{Deserialize, Serialize};

use crate::errors::{Error, Result};
use crate::planner::ResizePlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Prepared,
    BootingMaintenance,
    CheckingFilesystem,
    ShrinkingFilesystem,
    GrowingFilesystem,
    ResizingBlockDevice,
    ResizingPartition,
    Verifying,
    Completed,
    Failed,
    RecoveryRequired,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Prepared => "PREPARED",
            State::BootingMaintenance => "BOOTING_MAINTENANCE",
            State::CheckingFilesystem => "CHECKING_FILESYSTEM",
            State::ShrinkingFilesystem => "SHRINKING_FILESYSTEM",
            State::GrowingFilesystem => "GROWING_FILESYSTEM",
            State::ResizingBlockDevice => "RESIZING_BLOCK_DEVICE",
            State::ResizingPartition => "RESIZING_PARTITION",
            State::Verifying => "VERIFYING",
            State::Completed => "COMPLETED",
            State::Failed => "FAILED",
            State::RecoveryRequired => "RECOVERY_REQUIRED",
        }
    }

    pub fn is_abortable(&self) -> bool {
        matches!(self, State::Prepared | State::BootingMaintenance)
    }

    pub fn is_dangerous(&self) -> bool {
        matches!(
            self,
            State::ShrinkingFilesystem | State::ResizingBlockDevice | State::ResizingPartition
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub to: String,
    pub at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationState {
    pub op_id: String,
    pub state: State,
    pub plan: ResizePlan,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn state_dir() -> String {
    std::env::var("PARTRESIZE_STATE_DIR").unwrap_or_else(|_| "/var/lib/partresize".to_string())
}

pub struct StateStore {
    pub dir: String,
}

impl StateStore {
    pub fn new() -> Self {
        StateStore { dir: state_dir() }
    }

    fn path(&self) -> String {
        format!("{}/state.json", self.dir)
    }

    pub fn exists(&self) -> bool {
        std::path::Path::new(&self.path()).exists()
    }

    pub fn load(&self) -> Result<Option<OperationState>> {
        if !self.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(self.path())?;
        let op: OperationState = serde_json::from_str(&data)
            .map_err(|e| Error::State(format!("corrupt state file: {e}")))?;
        Ok(Some(op))
    }

    pub fn save(&self, op: &OperationState) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = format!("{}.tmp", self.path());
        let data = serde_json::to_string_pretty(op)?;
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, self.path())?;
        Ok(())
    }

    pub fn transition(&self, op: &mut OperationState, new_state: State) -> Result<()> {
        op.history.push(HistoryEntry {
            to: new_state.as_str().to_string(),
            at: now(),
        });
        op.state = new_state;
        self.save(op)
    }

    pub fn fail(&self, op: &mut OperationState, error: String) -> Result<()> {
        op.error = Some(error);
        self.transition(op, State::Failed)
    }

    pub fn clear(&self) -> Result<()> {
        if self.exists() {
            std::fs::remove_file(self.path())?;
        }
        Ok(())
    }

    /// Directory for this operation's backups (partition table, LVM metadata).
    pub fn backup_dir(&self, op_id: &str) -> Result<String> {
        let d = format!("{}/backups/{op_id}", self.dir);
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}

pub fn new_op_id() -> String {
    // YYYYmmdd-HHMMSS-ish from epoch seconds (no chrono dependency).
    let secs = now();
    format!("op-{secs}")
}
