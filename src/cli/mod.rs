//! Command-line interface: argument parsing, dispatch and output rendering.

use std::io::Write;
use std::process::ExitCode;

use crate::discovery;
use crate::errors::{Error, Result};
use crate::executor::{take_backups, Executor};
use crate::filesystem::capabilities_for;
use crate::planner::{build_plan, Direction, Request, ResizePlan};
use crate::recovery::{new_op_id, OperationState, State, StateStore};
use crate::safety;
use crate::schedule;
use crate::storage::Topology;
use crate::util::cmd::set_verbose;
use crate::util::sizes::{format_size, parse_size};

const USAGE: &str = "\
partresize — safety-first Linux partition & filesystem resizing

USAGE:
    partresize <command> [options]

COMMANDS:
    list                          List block devices
    inspect <target>              Show the storage topology of a target
    capabilities <target>         Show what the filesystem can/can't do
    plan <target> <size-opt>      Show the resize plan (never changes anything)
    resize <target> <size-opt>    Resize a filesystem/partition
    grow <target> [--by S|--size S]   Grow (to fill, or by/to a size)
    shrink <target> --size S|--by S   Shrink to/by a size
    backup-table <disk> [file]    Back up a partition table
    restore-table <disk> <file>   Restore a partition table
    status                        Show any in-progress operation
    cancel                        Cancel a scheduled operation
    recover                       Recover an interrupted operation

SIZE OPTIONS (for plan/resize):
    --size <S>     Absolute target size (e.g. 300G, 80GiB)
    --grow  <S>    Grow by S (omit S with `grow` to fill free space)
    --shrink <S>   Shrink by S
    --by <S>       Amount for grow/shrink subcommands

OPTIONS:
    --dry-run      Show what would happen; change nothing
    --offline      Perform an offline resize now (non-root: unmount/remount)
    --schedule     Prepare an offline resize for the next reboot
    -y, --yes      Do not prompt for confirmation
    -v, --verbose  Print each external command
    -h, --help     Show this help

Sizes accept binary (KiB/MiB/GiB/TiB) and decimal (KB/MB/GB/TB) units.
";

struct Parsed {
    command: String,
    positionals: Vec<String>,
    size: Option<String>,
    grow: Option<Option<String>>,
    shrink: Option<String>,
    by: Option<String>,
    dry_run: bool,
    offline: bool,
    schedule: bool,
    yes: bool,
    verbose: bool,
}

fn parse(args: &[String]) -> Result<Parsed> {
    let mut p = Parsed {
        command: String::new(),
        positionals: Vec::new(),
        size: None,
        grow: None,
        shrink: None,
        by: None,
        dry_run: false,
        offline: false,
        schedule: false,
        yes: false,
        verbose: false,
    };
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--size" => p.size = Some(next_val(&mut it, "--size")?),
            "--grow" => {
                // optional value: only consume if the next token isn't a flag
                if let Some(n) = it.peek() {
                    if !n.starts_with('-') && p.command == "grow" {
                        p.grow = Some(Some(it.next().unwrap().clone()));
                        continue;
                    }
                    if !n.starts_with('-') {
                        p.grow = Some(Some(it.next().unwrap().clone()));
                        continue;
                    }
                }
                p.grow = Some(None);
            }
            "--shrink" => p.shrink = Some(next_val(&mut it, "--shrink")?),
            "--by" => p.by = Some(next_val(&mut it, "--by")?),
            "--dry-run" => p.dry_run = true,
            "--offline" => p.offline = true,
            "--schedule" => p.schedule = true,
            "-y" | "--yes" => p.yes = true,
            "-v" | "--verbose" => p.verbose = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(Error::InvalidTarget(format!("unknown option {other}")))
            }
            other => {
                if p.command.is_empty() {
                    p.command = other.to_string();
                } else {
                    p.positionals.push(other.to_string());
                }
            }
        }
    }
    Ok(p)
}

fn next_val(
    it: &mut std::iter::Peekable<std::slice::Iter<'_, String>>,
    flag: &str,
) -> Result<String> {
    it.next()
        .cloned()
        .ok_or_else(|| Error::InvalidTarget(format!("{flag} requires a value")))
}

pub fn run(args: Vec<String>) -> Result<ExitCode> {
    if args.is_empty() {
        print!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let p = parse(&args)?;
    set_verbose(p.verbose);

    match p.command.as_str() {
        "list" => cmd_list(),
        "inspect" => cmd_inspect(&p),
        "capabilities" | "caps" => cmd_capabilities(&p),
        "plan" => cmd_plan(&p),
        "resize" | "grow" | "shrink" => cmd_resize(&p),
        "backup-table" => cmd_backup_table(&p),
        "restore-table" => cmd_restore_table(&p),
        "status" => cmd_status(),
        "cancel" => cmd_cancel(&p),
        "recover" => cmd_recover(&p),
        "run-scheduled" => cmd_run_scheduled(&p),
        "help" | "--help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(Error::InvalidTarget(format!(
            "unknown command {other:?}; run `partresize --help`"
        ))),
    }
}

fn target(p: &Parsed) -> Result<&str> {
    p.positionals.first().map(|s| s.as_str()).ok_or_else(|| {
        Error::InvalidTarget("this command requires a target (mount point or device)".into())
    })
}

fn resolve_request(p: &Parsed) -> Result<Request> {
    if let Some(s) = &p.size {
        return Ok(Request::Size(parse_size(s)?));
    }
    match p.command.as_str() {
        "shrink" => {
            if let Some(s) = &p.shrink {
                return Ok(Request::ShrinkBy(parse_size(s)?));
            }
            if let Some(s) = &p.by {
                return Ok(Request::ShrinkBy(parse_size(s)?));
            }
            Err(Error::InvalidTarget(
                "shrink requires --size or --by".into(),
            ))
        }
        "grow" => {
            if let Some(Some(s)) = &p.grow {
                return Ok(Request::GrowBy(parse_size(s)?));
            }
            if let Some(s) = &p.by {
                return Ok(Request::GrowBy(parse_size(s)?));
            }
            Ok(Request::GrowMax)
        }
        _ => {
            // resize / plan
            if let Some(opt) = &p.grow {
                return match opt {
                    Some(s) => Ok(Request::GrowBy(parse_size(s)?)),
                    None => Ok(Request::GrowMax),
                };
            }
            if let Some(s) = &p.shrink {
                return Ok(Request::ShrinkBy(parse_size(s)?));
            }
            Err(Error::InvalidTarget(
                "specify --size, --grow, or --shrink".into(),
            ))
        }
    }
}

// -- individual commands ---------------------------------------------------

fn cmd_list() -> Result<ExitCode> {
    print!("{}", discovery::list_block_devices()?);
    Ok(ExitCode::SUCCESS)
}

fn cmd_inspect(p: &Parsed) -> Result<ExitCode> {
    let t = target(p)?;
    let topo = discovery::discover(t)?;
    print_topology(&topo);
    if let Ok(pr) = crate::filesystem::probe(
        topo.fstype(),
        topo.fs_backing_device(),
        topo.mountpoint.as_deref(),
    ) {
        println!("Current filesystem size: {}", format_size(pr.size_bytes));
        println!("Used:                    {}", format_size(pr.used_bytes));
        if pr.min_size_bytes != 0 {
            println!(
                "Minimum shrink size:     {}",
                format_size(pr.min_size_bytes)
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_capabilities(p: &Parsed) -> Result<ExitCode> {
    let t = target(p)?;
    let topo = discovery::discover(t)?;
    let caps = capabilities_for(topo.fstype());
    println!("Filesystem: {}", caps.fstype);
    println!(
        "Mount point: {}",
        topo.mountpoint.as_deref().unwrap_or("(not mounted)")
    );
    println!("    online grow:    {}", yn(caps.online_grow));
    println!("    online shrink:  {}", yn(caps.online_shrink));
    println!("    offline grow:   {}", yn(caps.offline_grow));
    println!("    offline shrink: {}", yn(caps.offline_shrink));
    for n in &caps.notes {
        println!("    note: {n}");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_plan(p: &Parsed) -> Result<ExitCode> {
    let t = target(p)?;
    let req = resolve_request(p)?;
    let topo = discovery::discover(t)?;
    let plan = build_plan(topo, req)?;
    print_plan(&plan);
    println!("\nNo changes will be made.");
    Ok(if plan.feasible() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn cmd_resize(p: &Parsed) -> Result<ExitCode> {
    let t = target(p)?;
    let req = resolve_request(p)?;
    println!("Analyzing system...");
    let topo = discovery::discover(t)?;
    let plan = build_plan(topo, req)?;

    render_intro(&plan);

    if !plan.feasible() {
        println!("\nThis operation cannot proceed:");
        for b in &plan.blockers {
            println!("    - {b}");
        }
        // Give the actionable offline suggestion where relevant.
        return Ok(ExitCode::from(2));
    }
    if plan.direction == Direction::None {
        println!("Filesystem is already the requested size; nothing to do.");
        return Ok(ExitCode::SUCCESS);
    }

    if p.dry_run {
        return dry_run(&plan);
    }

    if !safety::is_root() {
        return Err(Error::Safety(
            "this operation requires root privileges (run with sudo)".into(),
        ));
    }

    // Decide execution mode.
    if plan.online && !p.offline && !p.schedule {
        return execute_online(&plan, p.yes);
    }

    if plan.online && (p.offline || p.schedule) {
        println!("(note: this operation could be done online; honouring your request)");
    }

    // Offline is required (or explicitly requested). If the target is mounted
    // and the user gave no offline/schedule choice, refuse rather than
    // surprise-unmount — and explain exactly how to proceed.
    let is_root_fs = plan.topology.mountpoint.as_deref() == Some("/");
    if plan.topology.mountpoint.is_some() && !p.offline && !p.schedule {
        let suggestion = if is_root_fs {
            format!(
                "partresize resize {} --size {} --schedule",
                plan.target,
                format_size(plan.target_fs_size)
            )
        } else {
            format!(
                "partresize resize {t} --size {s} --offline   (unmount now)\n    \
                 partresize resize {t} --size {s} --schedule  (do it at next reboot)",
                t = plan.target,
                s = format_size(plan.target_fs_size)
            )
        };
        return Err(Error::RequiresOffline {
            mountpoint: plan.topology.mountpoint.clone().unwrap_or_default(),
            fstype: plan.topology.fstype().to_string(),
            suggestion,
        });
    }

    if p.schedule || is_root_fs {
        return schedule_offline(&plan, p.yes);
    }
    execute_offline_now(&plan, p.yes)
}

fn cmd_backup_table(p: &Parsed) -> Result<ExitCode> {
    let disk = target(p)?;
    let topo_disk = resolve_disk(disk)?;
    let dest = p
        .positionals
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("./partition-table-{}.sfdisk", sanitise(&topo_disk)));
    crate::partition::backup_table(&topo_disk, &dest)?;
    println!("Partition table of {topo_disk} backed up to {dest}");
    println!("Restore with: partresize restore-table {topo_disk} {dest}");
    Ok(ExitCode::SUCCESS)
}

fn cmd_restore_table(p: &Parsed) -> Result<ExitCode> {
    let disk = target(p)?;
    let src = p
        .positionals
        .get(1)
        .ok_or_else(|| Error::InvalidTarget("restore-table requires a backup file".into()))?;
    let disk = resolve_disk(disk)?;
    println!("WARNING: restoring a partition table overwrites the current layout on {disk}.");
    if !p.dry_run && !confirm("Continue?", p.yes)? {
        return Err(Error::Aborted);
    }
    crate::partition::restore_table(&disk, src, p.dry_run)?;
    println!("Partition table restored on {disk}");
    Ok(ExitCode::SUCCESS)
}

fn cmd_status() -> Result<ExitCode> {
    let store = StateStore::new();
    match store.load()? {
        None => println!("No resize operation in progress."),
        Some(op) => {
            println!("Operation:  {}", op.op_id);
            println!("State:      {}", op.state.as_str());
            println!("Direction:  {:?}", op.plan.direction);
            println!("Target:     {}", format_size(op.plan.target_fs_size));
            if let Some(e) = &op.error {
                println!("Error:      {e}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_cancel(p: &Parsed) -> Result<ExitCode> {
    let store = StateStore::new();
    match store.load()? {
        None => println!("No operation in progress."),
        Some(op) => {
            if !op.state.is_abortable() {
                return Err(Error::State(format!(
                    "operation is in state {} and cannot be cancelled safely; use `recover`",
                    op.state.as_str()
                )));
            }
            let _ = schedule::cleanup(&op.op_id, p.dry_run);
            store.clear()?;
            println!("Cancelled operation {}", op.op_id);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_recover(p: &Parsed) -> Result<ExitCode> {
    let store = StateStore::new();
    let mut op = match store.load()? {
        None => {
            println!("No operation state found; nothing to recover.");
            return Ok(ExitCode::SUCCESS);
        }
        Some(op) => op,
    };
    let _ = schedule::cleanup(&op.op_id, p.dry_run);
    if op.state == State::Completed {
        store.clear()?;
        println!("Operation {} already completed; cleaned up.", op.op_id);
    } else if op.state.is_abortable() {
        store.clear()?;
        println!(
            "Operation {} had not started resizing; cleaned up safely.",
            op.op_id
        );
    } else if op.state.is_dangerous() {
        store.transition(&mut op, State::RecoveryRequired)?;
        println!(
            "Operation {} was interrupted during {}.\n\
             The filesystem is shrunk before the partition, so data is intact.\n\
             Re-run the resize to complete the partition step, or inspect manually.",
            op.op_id,
            op.state.as_str()
        );
    } else {
        store.transition(&mut op, State::Completed)?;
        store.clear()?;
        println!("Operation {} completed the resize; cleaned up.", op.op_id);
    }
    Ok(ExitCode::SUCCESS)
}

/// Hidden: execute a persisted PREPARED plan offline (used at maintenance boot).
fn cmd_run_scheduled(p: &Parsed) -> Result<ExitCode> {
    let store = StateStore::new();
    let mut op = store
        .load()?
        .ok_or_else(|| Error::State("no scheduled operation found".into()))?;
    let plan = op.plan.clone();
    {
        let mut ex = Executor::new(&plan, false, p.dry_run).with_state(&store, &mut op);
        ex.run()?;
    }
    store.transition(&mut op, State::Completed)?;
    store.clear()?;
    println!("Scheduled resize completed.");
    Ok(ExitCode::SUCCESS)
}

// -- execution helpers -----------------------------------------------------

fn execute_online(plan: &ResizePlan, yes: bool) -> Result<ExitCode> {
    println!(
        "Operation: ONLINE {}",
        dir_word(plan.direction).to_uppercase()
    );
    if !confirm("Perform online resize now?", yes)? {
        return Err(Error::Aborted);
    }
    let store = StateStore::new();
    let mut op = OperationState {
        op_id: new_op_id(),
        state: State::Prepared,
        plan: plan.clone(),
        history: vec![],
        error: None,
    };
    store.save(&op)?;
    {
        let mut ex = Executor::new(plan, true, false).with_state(&store, &mut op);
        ex.run()?;
    }
    store.transition(&mut op, State::Completed)?;
    store.clear()?;
    print!("{}", crate::verification::report_final(&plan.topology)?);
    println!("Resize completed successfully.");
    Ok(ExitCode::SUCCESS)
}

fn execute_offline_now(plan: &ResizePlan, yes: bool) -> Result<ExitCode> {
    print_destructive_warning(plan);
    if !confirm("Continue?", yes)? {
        return Err(Error::Aborted);
    }
    println!("Unmounting, resizing offline, and remounting...");
    schedule::run_offline_now(plan, false)?;
    print!("{}", crate::verification::report_final(&plan.topology)?);
    println!("Resize completed successfully.");
    Ok(ExitCode::SUCCESS)
}

fn schedule_offline(plan: &ResizePlan, yes: bool) -> Result<ExitCode> {
    if plan.direction == Direction::Shrink {
        println!("The filesystem cannot be safely shrunk while mounted.");
    } else {
        println!("This operation will be performed in a temporary maintenance environment.");
    }
    println!("A temporary offline resize environment will be created using");
    println!("your existing Linux installation. No USB or external OS is required.");
    println!("Resize plan:");
    for (i, op) in plan.operations.iter().enumerate() {
        println!("    {}. {}", i + 1, op.description);
    }
    print_destructive_warning(plan);
    if !confirm("Prepare this operation for the next reboot?", yes)? {
        return Err(Error::Aborted);
    }
    println!("\nPreparing resize environment...");
    let prepared = schedule::prepare_scheduled(plan, false)?;
    println!("Resize operation scheduled (id {}).", prepared.op_id);
    println!(
        "Boot integration: {} + {} (kernel cmdline marker: {}).",
        prepared.initramfs, prepared.bootloader, prepared.marker
    );
    println!("Maintenance image: {}", prepared.image_path);
    println!("Generated {} boot artefact(s).", prepared.files.len());
    println!("Reboot to perform the resize. Run `partresize cancel` to abort beforehand.");
    Ok(ExitCode::SUCCESS)
}

fn dry_run(plan: &ResizePlan) -> Result<ExitCode> {
    println!("\n--- DRY RUN (no changes will be made) ---");
    print_plan(plan);
    let store = StateStore::new();
    let bdir = format!("{}/backups/dry-run", store.dir);
    for line in take_backups(plan, &bdir, true)? {
        println!("    {line}");
    }
    // Show the exact operations that would run.
    println!("Operations that would execute:");
    let online = plan.online;
    let mut ex = Executor::new(plan, online, true);
    ex.run()?;
    println!("Dry run complete.");
    Ok(ExitCode::SUCCESS)
}

// -- rendering -------------------------------------------------------------

fn yn(b: bool) -> &'static str {
    if b {
        "YES"
    } else {
        "NO"
    }
}

fn dir_word(d: Direction) -> &'static str {
    match d {
        Direction::Grow => "grow",
        Direction::Shrink => "shrink",
        Direction::None => "none",
    }
}

fn print_topology(topo: &Topology) {
    println!("Storage topology:");
    print!("{}", topo.render_tree());
}

fn print_plan(plan: &ResizePlan) {
    print_topology(&plan.topology);
    if let Some(part) = plan.topology.partition() {
        println!("Current partition:  {}", format_size(part.size));
    }
    println!("Current filesystem: {}", format_size(plan.current_fs_size));
    println!("Used:               {}", format_size(plan.used_bytes));
    println!("Target:             {}", format_size(plan.target_fs_size));

    let label = if plan.direction == Direction::Shrink {
        "Online shrink"
    } else {
        "Online grow"
    };
    println!(
        "{label}: {}",
        if plan.online {
            "SUPPORTED"
        } else {
            "NOT SUPPORTED"
        }
    );
    println!("Execution strategy:");
    if plan.online {
        println!("    Immediate online resize (no reboot)");
    } else {
        println!("    Temporary initramfs maintenance environment");
    }
    println!("Required reboot:  {}", yn(plan.requires_reboot));
    println!("External media:   NOT REQUIRED");
    println!("Risk:             {}", plan.risk_level.as_str());
    println!("Operations:");
    for (i, op) in plan.operations.iter().enumerate() {
        println!("    {}. {}", i + 1, op.description);
    }
    for w in &plan.warnings {
        println!("Note: {w}");
    }
    if !plan.feasible() {
        println!("Status: CANNOT EXECUTE");
        for b in &plan.blockers {
            println!("    - {b}");
        }
    } else if plan.requires_reboot {
        println!("Status: SAFE TO EXECUTE OFFLINE");
    } else {
        println!("Status: SAFE TO EXECUTE ONLINE");
    }
}

fn render_intro(plan: &ResizePlan) {
    let topo = &plan.topology;
    let label = if topo.mountpoint.as_deref() == Some("/") {
        "Root filesystem:"
    } else {
        "Filesystem:"
    };
    println!("{label}");
    println!("    {}", topo.fs_backing_device());
    println!("    {}", topo.fstype());
    println!(
        "    Current filesystem size: {}",
        format_size(plan.current_fs_size)
    );
    println!("    Used: {}", format_size(plan.used_bytes));
    println!("    Target: {}", format_size(plan.target_fs_size));
}

fn print_destructive_warning(plan: &ResizePlan) {
    if !plan.destructive {
        return;
    }
    let topo = &plan.topology;
    println!("\nWARNING: This operation can cause permanent data loss.");
    println!("Device: {}", topo.fs_backing_device());
    println!("Filesystem: {}", topo.fstype());
    if let Some(mp) = &topo.mountpoint {
        println!("Mount point: {mp}");
    }
    println!("Current size: {}", format_size(plan.current_fs_size));
    println!("New size:     {}", format_size(plan.target_fs_size));
    println!(
        "Estimated data currently used: {}",
        format_size(plan.used_bytes)
    );
    println!("The filesystem will be checked, shrunk, and the underlying block device");
    println!("will then be reduced.");
}

fn confirm(prompt: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(false);
    }
    let a = line.trim().to_ascii_lowercase();
    Ok(a == "y" || a == "yes")
}

// -- helpers ---------------------------------------------------------------

fn resolve_disk(target: &str) -> Result<String> {
    // If it's already a disk device, use it; otherwise resolve via topology.
    if target.starts_with("/dev/") && std::path::Path::new(target).exists() {
        // Is it a whole disk? Ask lsblk for its parent, else itself.
        return Ok(target.to_string());
    }
    let topo = discovery::discover(target)?;
    Ok(topo.disk().device.clone())
}

fn sanitise(dev: &str) -> String {
    dev.trim_start_matches("/dev/").replace('/', "_")
}
