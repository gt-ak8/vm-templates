//! What `wbox` remembers about a sandbox between commands.
//!
//! The GitHub key ids are stored twice: as sandbox labels (queryable, moves
//! with the sandbox) and in a JSON file under the runtime home (survives a
//! sandbox record that is already gone when `destroy` runs).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Res, runtime};

/// Label carrying the GitHub authentication key id.
pub const AUTH_KEY_LABEL: &str = "wbox.auth_key_id";

/// Label carrying the GitHub signing key id.
pub const SIGNING_KEY_LABEL: &str = "wbox.signing_key_id";

/// The persisted record for one sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    /// Sandbox name.
    pub name: String,
    /// GitHub id of the registered authentication key.
    pub auth_key_id: Option<u64>,
    /// GitHub id of the registered signing key.
    pub signing_key_id: Option<u64>,
}

fn state_path(name: &str) -> Res<PathBuf> {
    Ok(runtime::home()?
        .join("wbox-state")
        .join(format!("{name}.json")))
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
