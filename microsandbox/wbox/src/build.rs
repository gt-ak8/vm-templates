//! `wbox build`: bake the devstation base snapshot.

use std::io::Write;

use microsandbox::{Sandbox, Snapshot, sandbox::SandboxStatus, sandbox::exec::ExecEvent};

use crate::{Res, preflight, runtime};

/// The snapshot every sandbox is cut from.
pub const BASE_SNAPSHOT: &str = "devstation-base";

/// Name of the throwaway sandbox the snapshot is taken from.
const BUILDER_SANDBOX: &str = "devstation-builder";

/// Base image the bootstrap runs on.
const BASE_IMAGE: &str = "debian:13";

/// `wbox build [--force] [--disk <gib>]`.
///
/// `disk_gib` is the only place the root disk is sized: a snapshot-rooted
/// sandbox inherits it and the SDK rejects `root_disk` on `create`.
pub async fn build(force: bool, disk_gib: u32) -> Res<()> {
    preflight::build_preflight()?;
    runtime::ensure_runtime().await?;

    if Snapshot::get(BASE_SNAPSHOT).await.is_ok() {
        if !force {
            println!("snapshot {BASE_SNAPSHOT} already exists; pass --force to rebuild");
            return Ok(());
        }
        eprintln!("wbox: removing the existing {BASE_SNAPSHOT} snapshot (--force)");
        Snapshot::remove(BASE_SNAPSHOT, true).await?;
    }

    if force {
        // A previous run that failed left its builder behind for inspection.
        // --force is the "start over" switch, so clear it out first.
        if let Ok(handle) = Sandbox::get(BUILDER_SANDBOX).await {
            eprintln!("wbox: removing the leftover {BUILDER_SANDBOX} sandbox (--force)");
            if matches!(
                handle.status_snapshot(),
                SandboxStatus::Running | SandboxStatus::Draining
            ) {
                handle.stop().await?;
            }
            Sandbox::remove(BUILDER_SANDBOX).await?;
        }
    }

    let payload = runtime::payload_dir();
    eprintln!("wbox: booting {BASE_IMAGE} as {BUILDER_SANDBOX} with a {disk_gib} GiB root disk");
    let sandbox = Sandbox::builder(BUILDER_SANDBOX)
        .image(BASE_IMAGE)
        .replace()
        .cpus(4)
        .memory(8192u32)
        .root_disk(disk_gib * 1024)
        // Deliberately unlabelled: `wbox list` shows the sandboxes a user
        // created, and this one is build scaffolding. `build --force` is what
        // cleans it up.
        .patch(|p| {
            p.copy_dir(payload.join("home"), "/opt/wbox/home", true)
                .copy_dir(payload.join("vm-files"), "/opt/wbox/vm-files", true)
                .copy_file(
                    payload.join("bootstrap.sh"),
                    "/opt/wbox/bootstrap.sh",
                    Some(0o755),
                    true,
                )
                .copy_file(
                    payload.join("provision.sh"),
                    "/opt/wbox/provision.sh",
                    Some(0o755),
                    true,
                )
        })
        .create()
        .await?;

    eprintln!("wbox: running /opt/wbox/bootstrap.sh (this takes a while)");
    let status = stream_exec(&sandbox, "/bin/bash", &["/opt/wbox/bootstrap.sh"]).await?;
    if status != 0 {
        // Leave the sandbox in place: its logs are the evidence.
        return Err(format!(
            "bootstrap.sh exited {status}; the {BUILDER_SANDBOX} sandbox was left for \
             inspection — `wbox build --force` starts over and removes it"
        )
        .into());
    }

    eprintln!("wbox: stopping {BUILDER_SANDBOX}");
    sandbox.stop().await?;

    eprintln!("wbox: snapshotting as {BASE_SNAPSHOT}");
    let handle = Sandbox::get(BUILDER_SANDBOX).await?;
    handle.snapshot(BASE_SNAPSHOT).await?;

    eprintln!("wbox: removing {BUILDER_SANDBOX}");
    Sandbox::remove(BUILDER_SANDBOX).await?;

    println!("snapshot {BASE_SNAPSHOT} ready");
    Ok(())
}

/// Run a command in the sandbox, forwarding its output live, and return its
/// exit code.
pub async fn stream_exec(sandbox: &Sandbox, cmd: &str, args: &[&str]) -> Res<i32> {
    let mut handle = sandbox.exec_stream(cmd, args.iter().copied()).await?;
    let mut code = -1;
    while let Some(event) = handle.recv().await {
        match event {
            ExecEvent::Stdout(bytes) | ExecEvent::Stderr(bytes) => {
                std::io::stderr().write_all(&bytes)?;
                std::io::stderr().flush()?;
            }
            ExecEvent::Exited { code: c } => code = c,
            ExecEvent::Failed(failure) => {
                return Err(format!("{cmd} failed to start: {failure:?}").into());
            }
            _ => {}
        }
    }
    Ok(code)
}
