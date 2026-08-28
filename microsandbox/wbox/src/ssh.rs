//! `wbox ssh` and `wbox ssh-proxy`.
//!
//! Both are in-process SSH: there is no sshd in the guest, and no listening
//! socket on the host. `ssh-proxy` is what the generated `~/.ssh/config.d/wbox`
//! entry runs as its `ProxyCommand`, so the ssh client speaks the protocol to
//! this process over stdin/stdout.

use microsandbox::{Sandbox, sandbox::SandboxStatus, sandbox::ssh::SshStdioStream};

use crate::{Res, runtime};

/// Return a running sandbox, starting it if it is stopped.
async fn running(name: &str) -> Res<Sandbox> {
    runtime::ensure_runtime().await?;
    let handle = Sandbox::get(name)
        .await
        .map_err(|error| format!("no sandbox named {name}: {error}"))?;
    match handle.status_snapshot() {
        SandboxStatus::Running | SandboxStatus::Draining => Ok(handle.connect().await?),
        _ => {
            eprintln!("wbox: starting {name}");
            Ok(handle.start_detached().await?)
        }
    }
}

/// `wbox ssh <name>`: an interactive shell as `dev`.
pub async fn ssh(name: &str) -> Res<()> {
    let sandbox = running(name).await?;
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
    let client = sandbox
        .ssh()
        .connect_with(|o| o.user("dev").term(term))
        .await?;
    let code = client.attach().await?;
    std::process::exit(code);
}

/// `wbox ssh-proxy <name>`: speak SSH over stdin/stdout.
///
/// Nothing may be written to stdout here but the SSH stream itself, so every
/// diagnostic goes to stderr.
pub async fn ssh_proxy(name: &str) -> Res<()> {
    let sandbox = running(name).await?;
    let server = sandbox.ssh().server_with(|o| o.user("dev")).await?;
    server.serve(SshStdioStream::new()).await?;
    Ok(())
}
