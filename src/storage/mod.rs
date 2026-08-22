//! The storage-stack data model.
//!
//! A topology is an ordered chain from the physical disk (top) down to the
//! filesystem (bottom):
//!
//! ```text
//! Disk -> Partition -> [LUKS] -> [LVM PV -> VG -> LV] -> Filesystem -> Mount
//! ```

use serde::{Deserialize, Serialize};

use crate::util::sizes::format_size;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayerKind {
    Disk,
    Partition,
    Luks,
    LvmPv,
    LvmVg,
    LvmLv,
    Filesystem,
}

/// One level of the storage stack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub kind: LayerKind,
    /// Kernel device path or logical name (e.g. VG name).
    pub device: String,
    /// Size of this layer in bytes.
    pub size: u64,
    // Optional, kind-specific attributes:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fstype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pttype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sector_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vg_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pv_device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mountpoint: Option<String>,
}

impl Layer {
    pub fn new(kind: LayerKind, device: impl Into<String>, size: u64) -> Self {
        Layer {
            kind,
            device: device.into(),
            size,
            fstype: None,
            used: None,
            pttype: None,
            part_number: None,
            part_start: None,
            sector_size: None,
            vg_name: None,
            pv_device: None,
            mountpoint: None,
        }
    }
}

/// The complete resolved stack for a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topology {
    /// The user-facing target that was resolved (mount point or device).
    pub target: String,
    /// Mount point of the bottom filesystem, if mounted.
    pub mountpoint: Option<String>,
    pub layers: Vec<Layer>,
}

impl Topology {
    pub fn first(&self, kind: LayerKind) -> Option<&Layer> {
        self.layers.iter().find(|l| l.kind == kind)
    }

    pub fn disk(&self) -> &Layer {
        self.first(LayerKind::Disk)
            .expect("topology invariant: a disk layer must exist")
    }

    pub fn partition(&self) -> Option<&Layer> {
        self.first(LayerKind::Partition)
    }

    pub fn luks(&self) -> Option<&Layer> {
        self.first(LayerKind::Luks)
    }

    pub fn lvm_lv(&self) -> Option<&Layer> {
        self.first(LayerKind::LvmLv)
    }

    pub fn lvm_vg(&self) -> Option<&Layer> {
        self.first(LayerKind::LvmVg)
    }

    pub fn filesystem(&self) -> &Layer {
        self.layers
            .last()
            .filter(|l| l.kind == LayerKind::Filesystem)
            .expect("topology invariant: a filesystem layer must exist at the bottom")
    }

    pub fn fstype(&self) -> &str {
        self.filesystem().fstype.as_deref().unwrap_or("")
    }

    /// The block device that directly contains the filesystem.
    pub fn fs_backing_device(&self) -> &str {
        &self.filesystem().device
    }

    pub fn has_lvm(&self) -> bool {
        self.lvm_lv().is_some()
    }

    pub fn has_luks(&self) -> bool {
        self.luks().is_some()
    }

    /// Render the topology as an indented tree.
    pub fn render_tree(&self) -> String {
        let mut out = String::new();
        for (i, layer) in self.layers.iter().enumerate() {
            let indent = "    ".repeat(i);
            let branch = if i == 0 { "" } else { "└── " };
            let extra = match layer.kind {
                LayerKind::Filesystem => {
                    format!(" [{}]", layer.fstype.as_deref().unwrap_or("?"))
                }
                LayerKind::LvmVg => format!(" (VG {})", layer.device),
                _ => String::new(),
            };
            out.push_str(&format!(
                "{indent}{branch}{}{extra}  {}\n",
                layer.device,
                format_size(layer.size)
            ));
        }
        if let Some(mp) = &self.mountpoint {
            let indent = "    ".repeat(self.layers.len());
            out.push_str(&format!("{indent}└── {mp}  (mount point)\n"));
        }
        out
    }
}
