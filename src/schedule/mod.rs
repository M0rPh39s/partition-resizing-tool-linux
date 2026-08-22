//! Offline execution and the reboot-assisted maintenance workflow.
//!
//! Two ways an offline operation reaches execution:
//!
//! * [`run_offline_now`] — for a *non-root* target: unmount, resize with the
//!   filesystem offline, then remount. Used by `resize --offline`.
//! * [`prepare_scheduled`] — for any target including `/`: build a maintenance
//!   environment from the system's own initramfs and schedule a one-time boot
//!   into it. Used by `resize --schedule`.
//!
//! Boot integration is distribution-independent: the initramfs generator
//! (initramfs-tools / dracut / mkinitcpio) and bootloader (GRUB / systemd-boot)
//! are detected at run time. All writes honour a prefix (for tests) and a
//! dry-run flag.

mod script;

use crate::errors::{Error, Result};
use crate::executor::{take_backups, Executor};
use crate::planner::ResizePlan;
use crate::recovery::{new_op_id, OperationState, State, StateStore};
use crate::util::cmd::{have, run_raw};

pub use script::generate_maintenance_script;

/// Execute an offline plan immediately on a *non-root*, currently-mounted or
/// unmounted target. Unmounts first if mounted, then remounts on success.
pub fn run_offline_now(plan: &ResizePlan, dry_run: bool) -> Result<()> {
    let topo = &plan.topology;
    if topo.mountpoint.as_deref() == Some("/") {
        return Err(Error::Unsupported(
            "the root filesystem cannot be taken offline from the running system; \
             use --schedule to perform this at the next reboot"
                .into(),
        ));
    }
    let remount = topo.mountpoint.clone();
    let fs_dev = topo.fs_backing_device().to_string();

    if let Some(mp) = &remount {
        if dry_run {
            eprintln!("[dry-run] umount {mp}");
        } else {
            run_raw(&["umount", mp], None).and_then(|o| {
                if o.ok() {
                    Ok(())
                } else {
                    Err(Error::Execution(format!(
                        "could not unmount {mp}: {}",
                        o.stderr.trim()
                    )))
                }
            })?;
        }
    }

    let store = StateStore::new();
    let mut op = OperationState {
        op_id: new_op_id(),
        state: State::Prepared,
        plan: plan.clone(),
        history: vec![],
        error: None,
    };
    if !dry_run {
        store.save(&op)?;
        let bdir = store.backup_dir(&op.op_id)?;
        for line in take_backups(plan, &bdir, false)? {
            eprintln!("  {line}");
        }
    }

    let result = {
        let mut ex = Executor::new(plan, false, dry_run).with_state(&store, &mut op);
        ex.run()
    };

    // Always attempt to remount, even on failure, to return the system to a
    // usable state.
    if let Some(mp) = &remount {
        if dry_run {
            eprintln!("[dry-run] mount {fs_dev} {mp}");
        } else {
            let _ = run_raw(&["mount", &fs_dev, mp], None);
        }
    }

    result?;
    if !dry_run {
        store.transition(&mut op, State::Completed)?;
        store.clear()?;
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Reboot-assisted scheduling
// -------------------------------------------------------------------------

#[allow(clippy::enum_variant_names)]
pub enum Initramfs {
    InitramfsTools,
    Dracut,
    Mkinitcpio,
}

pub enum Bootloader {
    Grub,
    SystemdBoot,
}

fn prefix() -> String {
    std::env::var("PARTRESIZE_PREFIX").unwrap_or_else(|_| "/".to_string())
}

fn under(prefix: &str, path: &str) -> String {
    let p = std::path::Path::new(prefix).join(path.trim_start_matches('/'));
    p.to_string_lossy().into_owned()
}

fn detect_initramfs(prefix: &str) -> Result<Initramfs> {
    if have("update-initramfs")
        || std::path::Path::new(&under(prefix, "etc/initramfs-tools")).is_dir()
    {
        Ok(Initramfs::InitramfsTools)
    } else if have("dracut") {
        Ok(Initramfs::Dracut)
    } else if have("mkinitcpio") {
        Ok(Initramfs::Mkinitcpio)
    } else {
        Err(Error::Unsupported(
            "no supported initramfs generator found (need initramfs-tools, dracut or mkinitcpio)"
                .into(),
        ))
    }
}

fn detect_bootloader(prefix: &str) -> Result<Bootloader> {
    if have("grub-reboot")
        || have("grub2-reboot")
        || std::path::Path::new(&under(prefix, "boot/grub")).is_dir()
        || std::path::Path::new(&under(prefix, "boot/grub2")).is_dir()
    {
        Ok(Bootloader::Grub)
    } else if std::path::Path::new(&under(prefix, "boot/loader/entries")).is_dir()
        || std::path::Path::new(&under(prefix, "efi/loader/entries")).is_dir()
        || (prefix == "/" && have("bootctl"))
    {
        Ok(Bootloader::SystemdBoot)
    } else {
        Err(Error::Unsupported(
            "no supported bootloader found (need GRUB or systemd-boot)".into(),
        ))
    }
}

fn write_file(path: &str, content: &str, mode: u32) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

fn kernel_version() -> String {
    run_raw(&["uname", "-r"], None)
        .ok()
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

pub struct Prepared {
    pub op_id: String,
    pub image_path: String,
    pub marker: String,
    pub initramfs: &'static str,
    pub bootloader: &'static str,
    pub files: Vec<String>,
}

/// Prepare a one-time maintenance boot that performs `plan` offline.
pub fn prepare_scheduled(plan: &ResizePlan, dry_run: bool) -> Result<Prepared> {
    let pfx = prefix();
    let ir = detect_initramfs(&pfx)?;
    let bl = detect_bootloader(&pfx)?;
    let op_id = new_op_id();
    let marker = format!("partresize.maint={op_id}");
    let kver = kernel_version();
    let image = format!("/boot/partresize-maint-{op_id}.img");
    let script = generate_maintenance_script(plan, &op_id);
    let bins = script::required_binaries(plan);

    // Persist state + backups before touching boot config.
    let store = StateStore::new();
    let mut op = OperationState {
        op_id: op_id.clone(),
        state: State::Prepared,
        plan: plan.clone(),
        history: vec![],
        error: None,
    };
    if !dry_run {
        store.save(&op)?;
        let bdir = store.backup_dir(&op_id)?;
        take_backups(plan, &bdir, false)?;
    }

    let mut files = Vec::new();
    let (ir_name, mut ir_files) =
        build_initramfs(&ir, &pfx, &op_id, &script, &bins, &image, &kver, dry_run)?;
    files.append(&mut ir_files);
    let (bl_name, mut bl_files) =
        schedule_boot(&bl, &pfx, &op_id, &image, &marker, &kver, dry_run)?;
    files.append(&mut bl_files);

    if !dry_run {
        store.transition(&mut op, State::BootingMaintenance)?;
    }

    Ok(Prepared {
        op_id,
        image_path: image,
        marker,
        initramfs: ir_name,
        bootloader: bl_name,
        files,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_initramfs(
    ir: &Initramfs,
    pfx: &str,
    op_id: &str,
    script: &str,
    bins: &[String],
    image: &str,
    kver: &str,
    dry_run: bool,
) -> Result<(&'static str, Vec<String>)> {
    let mut files = Vec::new();
    let marker = format!("partresize.maint={op_id}");
    match ir {
        Initramfs::InitramfsTools => {
            let premount = format!(
                "#!/bin/sh\nPREREQ=\"\"\nprereqs() {{ echo \"$PREREQ\"; }}\n\
                 case \"$1\" in prereqs) prereqs; exit 0;; esac\n\
                 for x in $(cat /proc/cmdline); do\n  case \"$x\" in\n    {marker}) /partresize/maint.sh; sync; reboot -f;;\n  esac\ndone\n"
            );
            let copies: String = bins
                .iter()
                .map(|b| {
                    format!(
                        "copy_exec /sbin/{b} /sbin 2>/dev/null || copy_exec /usr/sbin/{b} /sbin 2>/dev/null || copy_exec $(command -v {b} 2>/dev/null) /bin 2>/dev/null || true"
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let hook = format!(
                "#!/bin/sh\nPREREQ=\"\"\nprereqs() {{ echo \"$PREREQ\"; }}\n\
                 case \"$1\" in prereqs) prereqs; exit 0;; esac\n\
                 . /usr/share/initramfs-tools/hook-functions\n\
                 mkdir -p \"${{DESTDIR}}/partresize\"\n\
                 cp /etc/partresize/maint-{op_id}.sh \"${{DESTDIR}}/partresize/maint.sh\"\n\
                 chmod 755 \"${{DESTDIR}}/partresize/maint.sh\"\n{copies}\n"
            );
            let p1 = under(
                pfx,
                &format!("etc/initramfs-tools/scripts/local-premount/partresize-{op_id}"),
            );
            let p2 = under(
                pfx,
                &format!("etc/initramfs-tools/hooks/partresize-{op_id}"),
            );
            let p3 = under(pfx, &format!("etc/partresize/maint-{op_id}.sh"));
            write_file(&p1, &premount, 0o755)?;
            write_file(&p2, &hook, 0o755)?;
            write_file(&p3, script, 0o755)?;
            files.extend([p1, p2, p3]);
            if dry_run {
                eprintln!("[dry-run] mkinitramfs -o {} {kver}", under(pfx, image));
            } else {
                let _ = run_raw(&["mkinitramfs", "-o", &under(pfx, image), kver], None);
            }
            Ok(("initramfs-tools", files))
        }
        Initramfs::Dracut => {
            let moddir = under(
                pfx,
                &format!("usr/lib/dracut/modules.d/99partresize-{op_id}"),
            );
            let setup = format!(
                "#!/bin/bash\ncheck() {{ return 0; }}\ndepends() {{ echo \"dm crypt lvm\"; return 0; }}\n\
                 install() {{\n  inst_multiple {}\n  inst_simple \"$moddir/maint.sh\" /partresize/maint.sh\n  inst_hook pre-mount 99 \"$moddir/run.sh\"\n}}\n",
                bins.join(" ")
            );
            let runhook = format!(
                "#!/bin/sh\nfor x in $(cat /proc/cmdline); do\n  case \"$x\" in\n    {marker}) /partresize/maint.sh; sync; reboot -f;;\n  esac\ndone\n"
            );
            write_file(&format!("{moddir}/module-setup.sh"), &setup, 0o755)?;
            write_file(&format!("{moddir}/run.sh"), &runhook, 0o755)?;
            write_file(&format!("{moddir}/maint.sh"), script, 0o755)?;
            files.push(moddir.clone());
            if dry_run {
                eprintln!(
                    "[dry-run] dracut --force --add partresize-{op_id} {} {kver}",
                    under(pfx, image)
                );
            } else {
                let _ = run_raw(
                    &[
                        "dracut",
                        "--force",
                        "--add",
                        &format!("partresize-{op_id}"),
                        &under(pfx, image),
                        kver,
                    ],
                    None,
                );
            }
            Ok(("dracut", files))
        }
        Initramfs::Mkinitcpio => {
            let binadd: String = bins
                .iter()
                .map(|b| format!("    add_binary {b}"))
                .collect::<Vec<_>>()
                .join("\n");
            let install = format!(
                "#!/bin/bash\nbuild() {{\n{binadd}\n    add_file /etc/partresize/maint-{op_id}.sh /partresize/maint.sh 755\n    add_runscript\n}}\nhelp() {{ echo partresize; }}\n"
            );
            let runtime = format!(
                "#!/usr/bin/ash\nrun_hook() {{\n  for x in $(cat /proc/cmdline); do\n    case \"$x\" in\n      {marker}) /partresize/maint.sh; sync; reboot -f;;\n    esac\n  done\n}}\n"
            );
            let p1 = under(pfx, &format!("etc/initcpio/install/partresize-{op_id}"));
            let p2 = under(pfx, &format!("etc/initcpio/hooks/partresize-{op_id}"));
            let p3 = under(pfx, &format!("etc/partresize/maint-{op_id}.sh"));
            write_file(&p1, &install, 0o644)?;
            write_file(&p2, &runtime, 0o644)?;
            write_file(&p3, script, 0o755)?;
            files.extend([p1, p2, p3]);
            if dry_run {
                eprintln!(
                    "[dry-run] mkinitcpio -A partresize-{op_id} -g {}",
                    under(pfx, image)
                );
            } else {
                let _ = run_raw(
                    &[
                        "mkinitcpio",
                        "-A",
                        &format!("partresize-{op_id}"),
                        "-g",
                        &under(pfx, image),
                    ],
                    None,
                );
            }
            Ok(("mkinitcpio", files))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn schedule_boot(
    bl: &Bootloader,
    pfx: &str,
    op_id: &str,
    image: &str,
    marker: &str,
    kver: &str,
    dry_run: bool,
) -> Result<(&'static str, Vec<String>)> {
    let mut files = Vec::new();
    match bl {
        Bootloader::Grub => {
            let snippet = format!(
                "#!/bin/sh\nexec tail -n +3 $0\nmenuentry 'PartResize Maintenance' --id partresize-{op_id} {{\n  linux /boot/vmlinuz-{kver} root=/dev/ram0 {marker} ro\n  initrd {image}\n}}\n"
            );
            let path = under(pfx, &format!("etc/grub.d/09_partresize_{op_id}"));
            write_file(&path, &snippet, 0o755)?;
            files.push(path);
            let mkconfig = if have("grub-mkconfig") {
                "grub-mkconfig"
            } else {
                "grub2-mkconfig"
            };
            let cfg = if std::path::Path::new(&under(pfx, "boot/grub")).is_dir() {
                "boot/grub/grub.cfg"
            } else {
                "boot/grub2/grub.cfg"
            };
            let reboot_tool = if have("grub-reboot") {
                "grub-reboot"
            } else {
                "grub2-reboot"
            };
            if dry_run {
                eprintln!("[dry-run] {mkconfig} -o {}", under(pfx, cfg));
                eprintln!("[dry-run] {reboot_tool} partresize-{op_id}");
            } else {
                let _ = run_raw(&[mkconfig, "-o", &under(pfx, cfg)], None);
                let _ = run_raw(&[reboot_tool, &format!("partresize-{op_id}")], None);
            }
            Ok(("grub", files))
        }
        Bootloader::SystemdBoot => {
            let entry = format!(
                "title   PartResize Maintenance\nlinux   /vmlinuz-{kver}\ninitrd  {}\noptions root=/dev/ram0 {marker} ro\n",
                image.replace("/boot/", "/")
            );
            let path = under(pfx, &format!("boot/loader/entries/partresize-{op_id}.conf"));
            write_file(&path, &entry, 0o644)?;
            files.push(path);
            if dry_run {
                eprintln!("[dry-run] bootctl set-oneshot partresize-{op_id}.conf");
            } else {
                let _ = run_raw(
                    &[
                        "bootctl",
                        "set-oneshot",
                        &format!("partresize-{op_id}.conf"),
                    ],
                    None,
                );
            }
            Ok(("systemd-boot", files))
        }
    }
}

/// Remove any temporary maintenance boot artefacts for `op_id`.
pub fn cleanup(op_id: &str, dry_run: bool) -> Result<()> {
    let pfx = prefix();
    let candidates = [
        format!("etc/initramfs-tools/scripts/local-premount/partresize-{op_id}"),
        format!("etc/initramfs-tools/hooks/partresize-{op_id}"),
        format!("etc/partresize/maint-{op_id}.sh"),
        format!("etc/grub.d/09_partresize_{op_id}"),
        format!("boot/loader/entries/partresize-{op_id}.conf"),
        format!("boot/partresize-maint-{op_id}.img"),
        format!("etc/initcpio/install/partresize-{op_id}"),
        format!("etc/initcpio/hooks/partresize-{op_id}"),
    ];
    for c in candidates {
        let p = under(&pfx, &c);
        if std::path::Path::new(&p).exists() {
            if dry_run {
                eprintln!("[dry-run] rm {p}");
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    let moddir = under(
        &pfx,
        &format!("usr/lib/dracut/modules.d/99partresize-{op_id}"),
    );
    if std::path::Path::new(&moddir).is_dir() && !dry_run {
        let _ = std::fs::remove_dir_all(&moddir);
    }
    Ok(())
}
