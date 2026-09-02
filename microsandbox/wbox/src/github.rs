//! GitHub key registration through the `gh` CLI.
//!
//! Only the signing key is registered. Authentication is not an SSH concern
//! here: git in the sandbox reaches GitHub over HTTPS with `GH_TOKEN`, which
//! an SSO-enforced org authorizes once, whereas an SSH key would need
//! authorizing per sandbox through the web UI with no API to do it.
//!
//! Two tokens, deliberately: this module runs on the host and needs
//! `admin:ssh_signing_key` to add and remove the sandbox key. That authority
//! must not reach the guest, so it lives in `GH_ADMIN_TOKEN` and is passed to
//! `gh` per call. `GH_TOKEN` is the narrow token injected into the sandbox and
//! is never used here.
//!
//! The token is never logged, never passed as an argument (arguments are
//! world-readable in the process table) and never written to disk.

use std::process::Command;

use crate::Res;

/// Env var holding the host-side token that administers the keys.
pub const ADMIN_TOKEN_VAR: &str = "GH_ADMIN_TOKEN";

/// Env var holding the narrow token injected into the sandbox.
pub const SANDBOX_TOKEN_VAR: &str = "GH_TOKEN";

/// A `gh` invocation authenticated as the host admin token.
///
/// `GH_TOKEN` is overridden rather than left to be inherited: the process
/// environment carries the sandbox's narrow token under that name, and `gh`
/// would otherwise pick it up and fail on scope.
pub fn gh_admin(args: &[&str]) -> Res<Command> {
    let token = std::env::var(ADMIN_TOKEN_VAR).unwrap_or_default();
    if token.trim().is_empty() {
        return Err(format!("{ADMIN_TOKEN_VAR} is not set (add it to microsandbox/.env)").into());
    }
    let mut command = Command::new("gh");
    command
        .args(args)
        .env("GH_TOKEN", token)
        .env_remove("GITHUB_TOKEN");
    Ok(command)
}

/// Endpoint for commit-signing keys.
pub const SIGNING_KEYS_ENDPOINT: &str = "/user/ssh_signing_keys";

/// Register `pubkey` at `endpoint` under `title`; return the GitHub key id.
pub fn register_key(endpoint: &str, title: &str, pubkey: &str) -> Res<u64> {
    let output = gh_admin(&["api", "-X", "POST", endpoint, "-f"])?
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
///
/// GitHub answers 404 both for a key that does not exist and for a token
/// whose scopes do not cover the call, so a bare 404 cannot be read as
/// success: doing that reports a clean `destroy` while leaving the key on the
/// account. Every 404 is therefore confirmed with a read of the same path.
pub fn delete_key(endpoint: &str, id: u64) -> Res<()> {
    let path = format!("{endpoint}/{id}");
    let output = gh_admin(&["api", "-X", "DELETE", &path])?
        .output()
        .map_err(|e| format!("running gh (is it installed?): {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let failure = format!("gh api DELETE {path} failed: {}", stderr.trim());

    // gh surfaces GitHub's own hint when a scope is the cause. Trust it before
    // spending another request.
    if stderr.contains("scope") {
        return Err(failure.into());
    }
    if !(stderr.contains("404") || stderr.contains("Not Found")) {
        return Err(failure.into());
    }

    // The read needs only the `read:` half of the scope, which registering the
    // key already proved. If it still finds the key, the delete was refused.
    let probe = gh_admin(&["api", &path])?
        .output()
        .map_err(|e| format!("running gh (is it installed?): {e}"))?;
    if probe.status.success() {
        return Err(format!("{failure} (the key is still on the account)").into());
    }
    eprintln!("wbox: {path} was already gone");
    Ok(())
}
