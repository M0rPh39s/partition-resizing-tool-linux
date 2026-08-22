//! Integration tests driving the compiled `partresize` binary against
//! disposable loop-device disks.
//!
//! These are destructive by nature, so they run entirely on loop devices with
//! `PARTRESIZE_TEST_MODE=1` armed — the tool refuses any non-loop/dm device in
//! that mode, so a bug can never touch a real host disk. Tests self-skip when
//! not run as root or when `losetup`/`sfdisk` are unavailable.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_partresize");
const MIB: u64 = 1024 * 1024;

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("Uid:")
                    .map(|r| r.split_whitespace().next() == Some("0"))
            })
        })
        .unwrap_or(false)
}

fn have(tool: &str) -> bool {
    ["/sbin", "/usr/sbin", "/usr/local/sbin", "/bin", "/usr/bin"]
        .iter()
        .any(|d| Path::new(d).join(tool).is_file())
}

fn can_run() -> bool {
    is_root() && have("losetup") && have("sfdisk") && have("mkfs.ext4")
}

/// Whether the running kernel can mount a given filesystem type.
fn kernel_fs(fstype: &str) -> bool {
    std::fs::read_to_string("/proc/filesystems")
        .map(|s| {
            s.lines()
                .any(|l| l.split_whitespace().last() == Some(fstype))
        })
        .unwrap_or(false)
}

/// Whether the kernel device-mapper driver is usable (needed for LVM/LUKS).
fn dm_available() -> bool {
    have("dmsetup")
        && Command::new("dmsetup")
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn sh(cmd: &str) -> (bool, String) {
    let out = Command::new("sh").arg("-c").arg(cmd).output().unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

/// A disposable GPT/MBR loop disk that detaches itself on drop.
struct Disk {
    img: String,
    loop_dev: String,
}

impl Disk {
    fn new(size_mib: u64, label: &str, parts: &str) -> Self {
        let img = format!(
            "/tmp/partresize-it-{}.img",
            std::process::id() as u64 * 1000 + rand()
        );
        sh(&format!("truncate -s {}M {}", size_mib, img));
        let (_ok, out) = sh(&format!("losetup --show -f -P {}", img));
        let loop_dev = out.trim().to_string();
        let script = format!("label: {label}\\n\\n{parts}");
        sh(&format!(
            "printf '{script}' | sfdisk {loop_dev} >/dev/null 2>&1"
        ));
        sh(&format!(
            "partprobe {loop_dev}; udevadm settle 2>/dev/null; sleep 0.2"
        ));
        Disk { img, loop_dev }
    }

    fn part(&self, n: u32) -> String {
        format!("{}p{}", self.loop_dev, n)
    }
}

impl Drop for Disk {
    fn drop(&mut self) {
        sh(&format!(
            "umount {} 2>/dev/null; losetup -d {} 2>/dev/null",
            self.part(1),
            self.loop_dev
        ));
        let _ = std::fs::remove_file(&self.img);
    }
}

fn rand() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u64
}

struct Mount {
    dir: String,
}
impl Mount {
    fn new(dev: &str) -> Self {
        let dir = format!("/tmp/partresize-mnt-{}", rand());
        std::fs::create_dir_all(&dir).unwrap();
        sh(&format!("mount {dev} {dir}"));
        Mount { dir }
    }
}
impl Drop for Mount {
    fn drop(&mut self) {
        sh(&format!("umount {} 2>/dev/null", self.dir));
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn run(args: &[&str]) -> (bool, String) {
    let out = Command::new(BIN)
        .args(args)
        .env("PARTRESIZE_TEST_MODE", "1")
        .env("PARTRESIZE_MIN_MARGIN", (16 * MIB).to_string())
        .env(
            "PARTRESIZE_STATE_DIR",
            format!("/tmp/partresize-state-{}", rand()),
        )
        .output()
        .unwrap();
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), s)
}

fn fs_bytes(dev: &str) -> u64 {
    let (_ok, out) = sh(&format!("dumpe2fs -h {dev} 2>/dev/null"));
    let mut bc = 0u64;
    let mut bs = 0u64;
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("Block count:") {
            bc = v.trim().parse().unwrap_or(0);
        }
        if let Some(v) = line.strip_prefix("Block size:") {
            bs = v.trim().parse().unwrap_or(0);
        }
    }
    bc * bs
}

fn part_bytes(dev: &str) -> u64 {
    let (_ok, out) = sh(&format!("blockdev --getsz {dev}"));
    out.trim().parse::<u64>().unwrap_or(0) * 512
}

macro_rules! skip_unless_root {
    () => {
        if !can_run() {
            eprintln!("skipping: needs root + loop devices");
            return;
        }
    };
}

#[test]
fn ext4_offline_shrink_end_to_end() {
    skip_unless_root!();
    let disk = Disk::new(700, "gpt", "size=500MiB, type=linux\\ntype=linux");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    sh(&format!(
        "dd if=/dev/zero of={}/data bs=1M count=60 status=none",
        mp.dir
    ));

    let (ok, out) = run(&["resize", &mp.dir, "--size", "300M", "--offline", "-y"]);
    assert!(ok, "offline shrink failed: {out}");

    let fs = fs_bytes(&disk.part(1));
    let part = part_bytes(&disk.part(1));
    assert!(fs <= 300 * MIB, "fs not shrunk: {fs}");
    assert!(
        part >= fs,
        "partition smaller than filesystem: part={part} fs={fs}"
    );
    // data preserved and remounted
    let size = std::fs::metadata(format!("{}/data", mp.dir))
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(size, 60 * MIB, "data file changed size");
}

#[test]
fn refuses_mounted_shrink_without_flags() {
    skip_unless_root!();
    let disk = Disk::new(700, "gpt", "size=500MiB, type=linux\\ntype=linux");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    let (ok, out) = run(&["resize", &mp.dir, "--size", "300M", "-y"]);
    assert!(!ok, "should refuse mounted shrink");
    assert!(
        out.contains("offline"),
        "should explain offline requirement: {out}"
    );
}

#[test]
fn dry_run_changes_nothing() {
    skip_unless_root!();
    let disk = Disk::new(700, "gpt", "size=500MiB, type=linux\\ntype=linux");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    let before = part_bytes(&disk.part(1));
    let (ok, out) = run(&["resize", &mp.dir, "--size", "300M", "--dry-run"]);
    assert!(ok, "dry-run should succeed: {out}");
    assert_eq!(
        before,
        part_bytes(&disk.part(1)),
        "dry-run changed the partition"
    );
}

#[test]
fn ext4_offline_grow() {
    skip_unless_root!();
    // 200 MiB partition with free space after it; grow to 400 MiB.
    let disk = Disk::new(700, "gpt", "size=200MiB, type=linux");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    // grow requires offline execution in this sandbox (online ioctl blocked);
    // unmount first so the tool takes the offline path.
    drop(mp);
    let (ok, out) = run(&["resize", &disk.part(1), "--size", "400M", "--offline", "-y"]);
    assert!(ok, "offline grow failed: {out}");
    let fs = fs_bytes(&disk.part(1));
    assert!(fs >= 390 * MIB, "filesystem not grown: {fs}");
}

#[test]
fn insufficient_space_is_infeasible() {
    skip_unless_root!();
    let disk = Disk::new(700, "gpt", "size=500MiB, type=linux");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    sh(&format!(
        "dd if=/dev/zero of={}/big bs=1M count=200 status=none",
        mp.dir
    ));
    // target below used data must be refused by the planner (exit 2).
    let (ok, out) = run(&["plan", &mp.dir, "--size", "50M"]);
    assert!(!ok, "plan should be infeasible: {out}");
    assert!(
        out.contains("CANNOT EXECUTE") || out.to_lowercase().contains("destroy"),
        "{out}"
    );
}

#[test]
fn mbr_offline_shrink() {
    skip_unless_root!();
    let disk = Disk::new(700, "dos", "size=500MiB, type=83");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    sh(&format!(
        "dd if=/dev/zero of={}/data bs=1M count=40 status=none",
        mp.dir
    ));
    let (ok, out) = run(&["resize", &mp.dir, "--size", "300M", "--offline", "-y"]);
    assert!(ok, "mbr offline shrink failed: {out}");
    assert!(fs_bytes(&disk.part(1)) <= 300 * MIB);
}

#[test]
fn schedule_generates_boot_artifacts() {
    skip_unless_root!();
    if !have("mkinitramfs") && !have("dracut") && !have("mkinitcpio") {
        eprintln!("skipping: no initramfs generator");
        return;
    }
    let disk = Disk::new(700, "gpt", "size=500MiB, type=linux\\ntype=linux");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    sh(&format!(
        "dd if=/dev/zero of={}/data bs=1M count=40 status=none",
        mp.dir
    ));

    // Sandbox all boot writes under a prefix that looks like a GRUB system.
    let prefix = format!("/tmp/partresize-prefix-{}", rand());
    std::fs::create_dir_all(format!("{prefix}/boot/grub")).unwrap();
    let state = format!("/tmp/partresize-state-{}", rand());

    let out = Command::new(BIN)
        .args(["resize", &mp.dir, "--size", "300M", "--schedule", "-y"])
        .env("PARTRESIZE_TEST_MODE", "1")
        .env("PARTRESIZE_MIN_MARGIN", (16 * MIB).to_string())
        .env("PARTRESIZE_PREFIX", &prefix)
        .env("PARTRESIZE_STATE_DIR", &state)
        .output()
        .unwrap();
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "schedule failed: {log}");

    // A maintenance script and a GRUB one-shot snippet must have been written.
    let grubd = std::fs::read_dir(format!("{prefix}/etc/grub.d")).unwrap();
    assert!(
        grubd.into_iter().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("partresize")),
        "no grub.d snippet generated"
    );
    let maint = std::fs::read_dir(format!("{prefix}/etc/partresize")).unwrap();
    assert!(
        maint.into_iter().count() > 0,
        "no maintenance script generated"
    );
    // State advanced to BOOTING_MAINTENANCE.
    let s = std::fs::read_to_string(format!("{state}/state.json")).unwrap();
    assert!(s.contains("BootingMaintenance"), "state not advanced: {s}");

    let _ = std::fs::remove_dir_all(&prefix);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn inspect_and_capabilities_report_ext4() {
    skip_unless_root!();
    let disk = Disk::new(400, "gpt", "type=linux");
    sh(&format!("mkfs.ext4 -F -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    let (ok, out) = run(&["inspect", &mp.dir]);
    assert!(ok && out.contains("ext4"), "inspect: {out}");
    let (ok, out) = run(&["capabilities", &mp.dir]);
    assert!(ok, "capabilities failed");
    assert!(
        out.contains("online shrink:  NO"),
        "ext4 must not claim online shrink: {out}"
    );
}

#[test]
fn xfs_shrink_is_refused() {
    skip_unless_root!();
    if !have("mkfs.xfs") || !kernel_fs("xfs") {
        eprintln!("skipping: xfs unsupported on this kernel");
        return;
    }
    let disk = Disk::new(700, "gpt", "size=300MiB, type=linux");
    sh(&format!("mkfs.xfs -f -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    // XFS cannot shrink at all -> the plan must be infeasible.
    let (ok, out) = run(&["plan", &mp.dir, "--size", "200M"]);
    assert!(!ok, "xfs shrink must be refused: {out}");
    assert!(
        out.to_lowercase().contains("cannot be shrunk") || out.contains("CANNOT EXECUTE"),
        "{out}"
    );
}

#[test]
fn btrfs_is_discovered() {
    skip_unless_root!();
    if !have("mkfs.btrfs") || !kernel_fs("btrfs") {
        eprintln!("skipping: btrfs unsupported on this kernel");
        return;
    }
    let disk = Disk::new(700, "gpt", "size=500MiB, type=linux");
    sh(&format!("mkfs.btrfs -f -q {}", disk.part(1)));
    let mp = Mount::new(&disk.part(1));
    let (ok, out) = run(&["capabilities", &mp.dir]);
    assert!(ok, "btrfs capabilities failed: {out}");
    assert!(
        out.contains("online shrink:  YES"),
        "btrfs should allow online shrink: {out}"
    );
}

#[test]
fn luks_stack_is_discovered() {
    skip_unless_root!();
    if !have("cryptsetup") || !dm_available() {
        eprintln!("skipping: device-mapper/cryptsetup unavailable");
        return;
    }
    let disk = Disk::new(700, "gpt", "size=500MiB, type=linux");
    let part = disk.part(1);
    let name = format!("partresize-luks-{}", rand());
    sh(&format!(
        "printf 'testpass123' | cryptsetup luksFormat --type luks2 --pbkdf pbkdf2 \
         --pbkdf-force-iterations 1000 --batch-mode {part}"
    ));
    sh(&format!(
        "printf 'testpass123' | cryptsetup open {part} {name}"
    ));
    let mapper = format!("/dev/mapper/{name}");
    sh(&format!("mkfs.ext4 -F -q {mapper}"));
    let mp = Mount::new(&mapper);
    let (ok, out) = run(&["inspect", &mp.dir]);
    drop(mp);
    sh(&format!("cryptsetup close {name}"));
    assert!(ok, "luks inspect failed: {out}");
    assert!(
        out.contains("LUKS") || out.to_lowercase().contains("crypt"),
        "LUKS layer not shown: {out}"
    );
}

#[test]
fn lvm_stack_shrink_orders_lv_before_partition() {
    skip_unless_root!();
    if !have("lvcreate") || !dm_available() {
        eprintln!("skipping: device-mapper/LVM unavailable");
        return;
    }
    let disk = Disk::new(700, "gpt", "size=600MiB, type=linux");
    let part = disk.part(1);
    let vg = format!("prtvg{}", rand() % 100000);
    sh(&format!("pvcreate -f {part}"));
    sh(&format!("vgcreate {vg} {part}"));
    sh(&format!("lvcreate -y -L 400M -n root {vg}"));
    let mapper = format!("/dev/mapper/{vg}-root");
    sh(&format!("mkfs.ext4 -F -q {mapper}"));
    let mp = Mount::new(&mapper);
    sh(&format!(
        "dd if=/dev/zero of={}/d bs=1M count=40 status=none",
        mp.dir
    ));
    let (_ok, out) = run(&["plan", &mp.dir, "--size", "200M"]);
    drop(mp);
    sh(&format!("vgchange -an {vg} 2>/dev/null; vgremove -f {vg} 2>/dev/null; pvremove -f {part} 2>/dev/null"));
    // The LV reduce step must appear before the partition shrink step.
    let lv_idx = out.find("logical volume");
    let part_idx = out.find("Shrink the partition");
    if let (Some(a), Some(b)) = (lv_idx, part_idx) {
        assert!(a < b, "LV must be reduced before the partition: {out}");
    } else {
        panic!("expected LV and partition shrink steps in plan: {out}");
    }
}
