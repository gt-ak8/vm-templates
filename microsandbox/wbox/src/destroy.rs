//! `wbox destroy`: remove a sandbox and everything `create` registered for it.

use microsandbox::{Sandbox, sandbox::SandboxStatus};

use crate::{
    Res, create, github, runtime,
    state::{self, State},
};

/// `wbox destroy <name>`.
///
/// Never starts the sandbox: the key id comes from the state file, which is
/// readable whatever the sandbox is doing, and survives it entirely.
///
/// A GitHub failure does not block the local teardown. The sandbox and its
/// ssh block go regardless, and the state record is kept with the key id so a
/// later `destroy` of the same name retries the deletion.
pub async fn destroy(name: &str) -> Res<()> {
    runtime::ensure_runtime().await?;

    let handle = Sandbox::get(name).await.ok();
    let stored = state::load(name)?;
    if handle.is_none() && stored.is_none() {
        return Err(format!("no sandbox or state record named {name}").into());
    }

    let signing_key_id = stored.as_ref().and_then(|s| s.signing_key_id);

    let mut undeleted_key = None;
    if let Some(id) = signing_key_id {
        eprintln!("wbox: deleting the GitHub signing key");
        if let Err(error) = github::delete_key(github::SIGNING_KEYS_ENDPOINT, id) {
            eprintln!("wbox: {error}");
            undeleted_key = Some(id);
        }
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

    match undeleted_key {
        None => {
            state::remove(name)?;
            println!("sandbox {name} destroyed");
            Ok(())
        }
        Some(id) => {
            // The port is released: nothing listens on it any more. The key id
            // is what the retry needs.
            state::save(&State {
                name: name.to_string(),
                signing_key_id: Some(id),
                ssh_port: None,
            })?;
            Err(format!(
                "sandbox {name} removed, but its GitHub signing key {id} is still registered; \
                 fix {} or the connection and run `wbox destroy {name}` again to delete it",
                github::ADMIN_TOKEN_VAR
            )
            .into())
        }
    }
}

/// Drop the `Host wbox-<name>` block from the wbox ssh config fragment.
fn drop_ssh_config(name: &str) -> Res<()> {
    let path = create::ssh_config_path()?;
    let contents = create::read_or_empty(&path)?;
    if contents.is_empty() {
        return Ok(());
    }
    create::write_private(&path, &create::drop_ssh_block(&contents, name))
}
