//! `wbox ssh`, `wbox start`, `wbox stop`.
//!
//! The guest runs a real sshd on port 22. `create` publishes it on a loopback
//! port of the host (see the `ssh_port` in the state file and the generated
//! `~/.ssh/config.d/wbox` block), so the ssh client talks TCP to the runtime's
//! port forwarder, not to this process. Consoles over the agent channel are
//! gone: they stalled the keystroke stream every second or so.
//!
//! The guest has no init, so sshd is started here, by whichever command boots
//! the sandbox.

use std::os::unix::process::CommandExt;

use microsandbox::{Sandbox, sandbox::SandboxHandle, sandbox::SandboxStatus};

use crate::{Res, runtime, state};

/// What the guest runs to bring sshd up if it is not already listening.
///
/// Debian's sshd refuses to start without its privilege-separation dir, and
/// `/run` starts empty on every boot.
const SSHD_START: &str =
    "sudo mkdir -p /run/sshd && (pgrep -x sshd >/dev/null || sudo /usr/sbin/sshd)";

fn is_running(handle: &SandboxHandle) -> bool {
    matches!(
        handle.status_snapshot(),
        SandboxStatus::Running | SandboxStatus::Draining
    )
}

async fn get(name: &str) -> Res<SandboxHandle> {
    runtime::ensure_runtime().await?;
    Sandbox::get(name)
        .await
        .map_err(|error| format!("no sandbox named {name}: {error}").into())
}

/// `wbox ssh <name>`: hand over to `ssh wbox-<name>`.
///
/// Does not start a stopped sandbox: booting one is a few seconds of side
/// effect nobody asked for when they only wanted to check whether it was up.
pub async fn ssh(name: &str) -> Res<()> {
    let handle = get(name).await?;
    if !is_running(&handle) {
        return Err(format!("{name} is not running; `wbox start {name}` first").into());
    }
    let error = std::process::Command::new("ssh")
        .arg(format!("wbox-{name}"))
        .exec();
    Err(format!("cannot run ssh: {error}").into())
}

/// `wbox start <name>`: boot the sandbox and its sshd.
pub async fn start(name: &str) -> Res<()> {
    let handle = get(name).await?;
    let sandbox = if is_running(&handle) {
        eprintln!("wbox: {name} is already running");
        handle.connect().await?
    } else {
        eprintln!("wbox: starting {name}");
        handle.start_detached().await?
    };
    ensure_sshd(&sandbox).await?;
    match state::load(name)?.and_then(|s| s.ssh_port) {
        Some(port) => println!("sandbox {name} running: ssh wbox-{name} (127.0.0.1:{port})"),
        None => println!("sandbox {name} running (no ssh port on record)"),
    }
    Ok(())
}

/// `wbox stop <name>`: stop the sandbox, keeping its disk and its records.
pub async fn stop(name: &str) -> Res<()> {
    let handle = get(name).await?;
    if !is_running(&handle) {
        println!("sandbox {name} is already stopped");
        return Ok(());
    }
    eprintln!("wbox: stopping {name}");
    handle.stop().await?;
    println!("sandbox {name} stopped");
    Ok(())
}

/// Start sshd in the guest unless it is already listening.
pub async fn ensure_sshd(sandbox: &Sandbox) -> Res<()> {
    let output = sandbox
        .exec_with("/bin/bash", |e| e.args(["-c", SSHD_START]).user("dev"))
        .await?;
    if !output.status().success {
        return Err(format!(
            "starting sshd in the guest failed (exit {}): {}",
            output.status().code,
            output.stderr()?.trim()
        )
        .into());
    }
    Ok(())
}
