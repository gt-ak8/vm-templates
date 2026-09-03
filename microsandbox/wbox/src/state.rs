//! What `wbox` remembers about a sandbox between commands.
//!
//! The GitHub signing key id and the published ssh port live in a JSON file
//! under the runtime home. The id cannot be a sandbox label: a label is only
//! settable before boot, and the id does not exist until `provision.sh` has
//! generated the key inside a running sandbox. The port is here so `create`
//! can see which ports its siblings hold, running or stopped.
//! The file also outlives a sandbox record that is already gone when `destroy`
//! runs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Res, runtime};

/// The persisted record for one sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    /// Sandbox name.
    pub name: String,
    /// GitHub id of the registered signing key.
    pub signing_key_id: Option<u64>,
    /// Host loopback port the guest's sshd is published on.
    #[serde(default)]
    pub ssh_port: Option<u16>,
}

fn state_dir() -> Res<PathBuf> {
    Ok(runtime::home()?.join("wbox-state"))
}

fn state_path(name: &str) -> Res<PathBuf> {
    Ok(state_dir()?.join(format!("{name}.json")))
}

/// Every record there is, in no particular order.
pub fn all() -> Res<Vec<State>> {
    let entries = match std::fs::read_dir(state_dir()?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut states = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            states.push(serde_json::from_slice(&std::fs::read(&path)?)?);
        }
    }
    Ok(states)
}

/// Write the record for `state.name`.
pub fn save(state: &State) -> Res<()> {
    let path = state_path(&state.name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(state)?)?;
    Ok(())
}

/// Read the record for `name`, if there is one.
pub fn load(name: &str) -> Res<Option<State>> {
    let path = state_path(name)?;
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Drop the record for `name`.
pub fn remove(name: &str) -> Res<()> {
    let path = state_path(name)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
