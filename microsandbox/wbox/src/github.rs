//! GitHub key registration through the `gh` CLI.
//!
//! `gh` is invoked with `GH_TOKEN` inherited from this process. The token is
//! never logged, never passed as an argument (arguments are world-readable in
//! the process table) and never written to disk.

use std::process::Command;

use crate::Res;

/// Endpoint for authentication (push/pull) keys.
pub const AUTH_KEYS_ENDPOINT: &str = "/user/keys";

/// Endpoint for commit-signing keys.
pub const SIGNING_KEYS_ENDPOINT: &str = "/user/ssh_signing_keys";

/// Register `pubkey` at `endpoint` under `title`; return the GitHub key id.
pub fn register_key(endpoint: &str, title: &str, pubkey: &str) -> Res<u64> {
    let output = Command::new("gh")
        .args(["api", "-X", "POST", endpoint, "-f"])
        .arg(format!("title={title}"))
        .arg("-f")
        .arg(format!("key={pubkey}"))
        .output()
        .map_err(|e| format!("running gh (is it installed?): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh api POST {endpoint} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    body.get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("gh api POST {endpoint} returned no key id").into())
}

/// Delete key `id` at `endpoint`. A key that is already gone is not an error.
pub fn delete_key(endpoint: &str, id: u64) -> Res<()> {
    let output = Command::new("gh")
        .args(["api", "-X", "DELETE"])
        .arg(format!("{endpoint}/{id}"))
        .output()
        .map_err(|e| format!("running gh (is it installed?): {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("404") || stderr.contains("Not Found") {
        eprintln!("wbox: {endpoint}/{id} was already gone");
        return Ok(());
    }
    Err(format!("gh api DELETE {endpoint}/{id} failed: {}", stderr.trim()).into())
}
