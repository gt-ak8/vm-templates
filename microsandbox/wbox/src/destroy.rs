//! `wbox destroy`: remove a sandbox and everything `create` registered for it.

use std::collections::BTreeMap;

use microsandbox::{Sandbox, sandbox::SandboxStatus};

use crate::{
    Res, create, github, runtime,
    state::{self, AUTH_KEY_LABEL, SIGNING_KEY_LABEL},
};

/// `wbox destroy <name>`.
///
/// Never starts the sandbox: the key ids come from the persisted labels or
/// from the state file, both readable while it is stopped.
pub async fn destroy(name: &str) -> Res<()> {
    runtime::ensure_runtime().await?;

    let handle = Sandbox::get(name).await.ok();
    let labels: BTreeMap<String, String> = match &handle {
        Some(handle) => handle.config()?.spec.labels.clone(),
        None => BTreeMap::new(),
    };
    let stored = state::load(name)?;
    if handle.is_none() && stored.is_none() {
        return Err(format!("no sandbox or state record named {name}").into());
    }

    let auth_key_id =
        key_id(&labels, AUTH_KEY_LABEL).or(stored.as_ref().and_then(|s| s.auth_key_id));
    let signing_key_id =
        key_id(&labels, SIGNING_KEY_LABEL).or(stored.as_ref().and_then(|s| s.signing_key_id));

    if let Some(id) = auth_key_id {
        eprintln!("wbox: deleting the GitHub authentication key");
        github::delete_key(github::AUTH_KEYS_ENDPOINT, id)?;
    }
    if let Some(id) = signing_key_id {
        eprintln!("wbox: deleting the GitHub signing key");
        github::delete_key(github::SIGNING_KEYS_ENDPOINT, id)?;
    }

    if let Some(handle) = &handle {
        if matches!(
            handle.status_snapshot(),
            SandboxStatus::Running | SandboxStatus::Draining
        ) {
            eprintln!("wbox: stopping {name}");
            handle.stop().await?;
        }
        eprintln!("wbox: removing {name}");
        Sandbox::remove(name).await?;
    }

    drop_ssh_config(name)?;
    state::remove(name)?;
    println!("sandbox {name} destroyed");
    Ok(())
}

fn key_id(labels: &BTreeMap<String, String>, key: &str) -> Option<u64> {
    labels.get(key).and_then(|value| value.parse().ok())
}

/// Drop the `Host wbox-<name>` block from the wbox ssh config fragment.
fn drop_ssh_config(name: &str) -> Res<()> {
    let path = create::ssh_config_path()?;
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let header = format!("Host wbox-{name}");
    let mut kept = String::new();
    let mut skipping = false;
    for line in contents.lines() {
        if line.trim_start().starts_with("Host ") {
            skipping = line.trim() == header;
        }
        if !skipping {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    std::fs::write(&path, kept)?;
    Ok(())
}
