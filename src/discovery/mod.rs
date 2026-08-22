//! Discovery of the complete storage stack behind a target.
//!
//! Walks the block-device tree with `lsblk -J` and resolves the mount source
//! with `findmnt`, then classifies each node into a [`Layer`]. Filesystem type
//! is confirmed via `findmnt`/`blkid` because lsblk's FSTYPE column depends on
//! a populated blkid cache that is not always present.

use serde::Deserialize;

use crate::errors::{Error, Result};
use crate::lvm;
use crate::storage::{Layer, LayerKind, Topology};
use crate::util::cmd::{have, require, run_raw};

const LSBLK_COLS: &str =
    "NAME,KNAME,PATH,TYPE,FSTYPE,SIZE,FSSIZE,FSUSED,START,PARTN,PTTYPE,MOUNTPOINT,PKNAME,LOG-SEC";

#[derive(Deserialize)]
struct LsblkRoot {
    #[serde(default)]
    blockdevices: Vec<Node>,
}

#[derive(Deserialize, Clone)]
struct Node {
    #[serde(default)]
    kname: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(rename = "type", default)]
    ntype: Option<String>,
    #[serde(default)]
    fstype: Option<String>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    size: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    fssize: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    fsused: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    start: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    partn: Option<u64>,
    #[serde(default)]
    pttype: Option<String>,
    #[serde(rename = "log-sec", default, deserialize_with = "de_opt_u64")]
    log_sec: Option<u64>,
    #[serde(default)]
    children: Vec<Node>,
}

impl Node {
    fn dev(&self) -> String {
        self.path
            .clone()
            .or_else(|| self.kname.as_ref().map(|k| format!("/dev/{k}")))
            .unwrap_or_default()
    }
}

/// Accept a JSON number, numeric string, or null as an Option<u64>.
fn de_opt_u64<'de, D>(d: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => Ok(n.as_u64()),
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse().map(Some).map_err(D::Error::custom)
            }
        }
        _ => Ok(None),
    }
}

fn canonical(dev: &str) -> String {
    std::fs::canonicalize(dev)
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| dev.to_string())
}

/// Resolve a user target (mount point or device) to (canonical_source, mount).
fn resolve_source(target: &str) -> Result<(String, Option<String>)> {
    require(&["findmnt"])?;
    // If the target is a block device, use it directly and find its mount.
    if target.starts_with("/dev/") && std::path::Path::new(target).exists() {
        let mount = run_raw(&["findmnt", "-n", "-o", "TARGET", "--source", target], None)
            .ok()
            .filter(|o| o.ok())
            .and_then(|o| o.stdout.lines().next().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty());
        return Ok((canonical(target), mount));
    }
    // Otherwise treat it as a path and resolve the backing device.
    let out = run_raw(&["findmnt", "-n", "-o", "SOURCE", "--target", target], None)?;
    let src = out
        .stdout
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::InvalidTarget(format!("could not resolve {target:?} to a device")))?;
    Ok((canonical(&src), Some(target.to_string())))
}

/// Depth-first search returning the ancestor chain [disk, .., matched].
fn find_chain<'a>(nodes: &'a [Node], target: &str, trail: &mut Vec<&'a Node>) -> bool {
    for node in nodes {
        trail.push(node);
        let matches = canonical(&node.dev()) == target
            || node
                .kname
                .as_ref()
                .map(|k| canonical(&format!("/dev/{k}")) == target)
                .unwrap_or(false);
        if matches || find_chain(&node.children, target, trail) {
            return true;
        }
        trail.pop();
    }
    false
}

fn resolve_fstype(device: &str, mountpoint: Option<&str>) -> String {
    if let Some(mp) = mountpoint {
        if let Ok(o) = run_raw(&["findmnt", "-n", "-o", "FSTYPE", "--target", mp], None) {
            if o.ok() {
                if let Some(l) = o.stdout.lines().next() {
                    if !l.trim().is_empty() {
                        return l.trim().to_string();
                    }
                }
            }
        }
    }
    if have("blkid") {
        if let Ok(o) = run_raw(&["blkid", "-o", "value", "-s", "TYPE", device], None) {
            if o.ok() && !o.stdout.trim().is_empty() {
                return o.stdout.trim().to_string();
            }
        }
    }
    String::new()
}

fn classify(node: &Node, is_top: bool, is_bottom: bool, bottom_fstype: &str) -> Vec<Layer> {
    let ntype = node.ntype.as_deref().unwrap_or("");
    let dev = node.dev();
    let size = node.size.unwrap_or(0);
    let mut layers = Vec::new();

    match ntype {
        "disk" | "loop" if is_top => {
            let mut l = Layer::new(LayerKind::Disk, &dev, size);
            l.pttype = node.pttype.clone();
            l.sector_size = Some(node.log_sec.unwrap_or(512));
            layers.push(l);
        }
        "part" => {
            let mut l = Layer::new(LayerKind::Partition, &dev, size);
            l.part_number = node.partn.map(|n| n as u32);
            l.part_start = node.start;
            l.pttype = node.pttype.clone();
            layers.push(l);
        }
        "crypt" => layers.push(Layer::new(LayerKind::Luks, &dev, size)),
        "lvm" => {
            let (pv, vg) = lvm::pv_and_vg(&dev);
            if let Some(vg) = vg.clone() {
                let mut vgl = Layer::new(LayerKind::LvmVg, &vg, size);
                vgl.pv_device = pv.clone();
                vgl.vg_name = Some(vg);
                layers.push(vgl);
            }
            let mut lvl = Layer::new(LayerKind::LvmLv, &dev, size);
            lvl.vg_name = vg;
            layers.push(lvl);
        }
        other => {
            // Unknown intermediate layer: represent generically so the planner
            // can refuse rather than mishandle it.
            let mut l = Layer::new(LayerKind::Partition, &dev, size);
            l.pttype = Some(format!("raw:{other}"));
            layers.push(l);
        }
    }

    if is_bottom {
        let fstype = node
            .fstype
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| bottom_fstype.to_string());
        if !fstype.is_empty() {
            let mut fs = Layer::new(LayerKind::Filesystem, &dev, node.fssize.unwrap_or(size));
            fs.fstype = Some(fstype);
            fs.used = node.fsused;
            layers.push(fs);
        }
    }
    layers
}

pub fn discover(target: &str) -> Result<Topology> {
    require(&["lsblk"])?;
    let (source, mount) = resolve_source(target)?;

    let out = run_raw(&["lsblk", "-J", "-b", "-o", LSBLK_COLS], None)?;
    if !out.ok() {
        return Err(Error::Command {
            program: "lsblk -J".into(),
            code: out.status,
            stderr: out.stderr,
        });
    }
    let root: LsblkRoot = serde_json::from_str(&out.stdout)?;

    let mut trail: Vec<&Node> = Vec::new();
    if !find_chain(&root.blockdevices, &source, &mut trail) {
        return Err(Error::Discovery(format!(
            "could not locate {source} ({target}) in the block-device tree"
        )));
    }

    let bottom = trail.last().unwrap();
    let bottom_dev = bottom.dev();
    let bottom_fstype = resolve_fstype(&bottom_dev, mount.as_deref());

    let mut layers = Vec::new();
    let n = trail.len();
    for (i, node) in trail.iter().enumerate() {
        layers.extend(classify(node, i == 0, i == n - 1, &bottom_fstype));
    }

    let topo = Topology {
        target: target.to_string(),
        mountpoint: mount,
        layers,
    };
    if topo.layers.first().map(|l| l.kind) != Some(LayerKind::Disk) {
        return Err(Error::Discovery(
            "could not resolve the physical disk for the stack".into(),
        ));
    }
    if topo.layers.last().map(|l| l.kind) != Some(LayerKind::Filesystem) {
        return Err(Error::Discovery(format!(
            "no filesystem detected on {source}; unable to plan a resize"
        )));
    }
    Ok(topo)
}

/// List all disks and their partitions (for `partresize list`).
pub fn list_block_devices() -> Result<String> {
    require(&["lsblk"])?;
    let out = run_raw(
        &[
            "lsblk",
            "-o",
            "NAME,SIZE,TYPE,FSTYPE,MOUNTPOINT",
            "-e",
            "7", // exclude loop-on-squashfs (snap) noise via major 7? keep simple
        ],
        None,
    );
    match out {
        Ok(o) if o.ok() => Ok(o.stdout),
        _ => Ok(crate::util::cmd::capture(&[
            "lsblk",
            "-o",
            "NAME,SIZE,TYPE,FSTYPE,MOUNTPOINT",
        ])?),
    }
}
