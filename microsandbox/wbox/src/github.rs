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
/// account. Every 404 is therefore confirmed with a read of the same path
/// (see `triage_delete` and `after_probe`).
pub fn delete_key(endpoint: &str, id: u64) -> Res<()> {
    let path = format!("{endpoint}/{id}");
    let output = gh_admin(&["api", "-X", "DELETE", &path])?
        .output()
        .map_err(|e| format!("running gh (is it installed?): {e}"))?;
    let failure = match triage_delete(&path, output.status.success(), &output.stderr) {
        DeleteVerdict::Deleted => return Ok(()),
        DeleteVerdict::Failed(failure) => return Err(failure.into()),
        DeleteVerdict::Probe(failure) => failure,
    };

    // The read needs only the `read:` half of the scope, which registering the
    // key already proved. If it still finds the key, the delete was refused.
    let probe = gh_admin(&["api", &path])?
        .output()
        .map_err(|e| format!("running gh (is it installed?): {e}"))?;
    after_probe(&path, &failure, probe.status.success())
}

/// What a `DELETE` reply means, before any further request.
#[derive(Debug, PartialEq, Eq)]
pub enum DeleteVerdict {
    /// GitHub deleted the key.
    Deleted,
    /// The delete failed for a reason a probe would not change.
    Failed(String),
    /// A 404: gone or refused, and only a read of the path can tell. Carries
    /// the failure message for the refused case.
    Probe(String),
}

/// Read a `DELETE` outcome from `gh`'s exit status and stderr.
///
/// gh surfaces GitHub's own hint when a scope is the cause, so that is trusted
/// before another request is spent. Anything but a 404 is final.
pub fn triage_delete(path: &str, success: bool, stderr: &[u8]) -> DeleteVerdict {
    if success {
        return DeleteVerdict::Deleted;
    }
    let stderr = String::from_utf8_lossy(stderr);
    let failure = format!("gh api DELETE {path} failed: {}", stderr.trim());
    if stderr.contains("scope") {
        return DeleteVerdict::Failed(failure);
    }
    if !(stderr.contains("404") || stderr.contains("Not Found")) {
        return DeleteVerdict::Failed(failure);
    }
    DeleteVerdict::Probe(failure)
}

/// Resolve a 404 with the result of reading the same path: a key that is still
/// readable was not deleted, one that is not was already gone.
pub fn after_probe(path: &str, failure: &str, key_still_readable: bool) -> Res<()> {
    if key_still_readable {
        return Err(format!("{failure} (the key is still on the account)").into());
    }
    eprintln!("wbox: {path} was already gone");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DeleteVerdict, after_probe, triage_delete};

    const PATH: &str = "/user/ssh_signing_keys/7";

    #[test]
    fn a_successful_delete_is_final() {
        assert_eq!(triage_delete(PATH, true, b""), DeleteVerdict::Deleted);
    }

    #[test]
    fn a_scope_hint_is_a_failure_without_a_probe() {
        let stderr =
            b"HTTP 404: Not Found. This API operation needs the \"admin:ssh_signing_key\" scope.";
        assert!(matches!(
            triage_delete(PATH, false, stderr),
            DeleteVerdict::Failed(_)
        ));
    }

    #[test]
    fn a_server_error_is_a_failure_without_a_probe() {
        assert!(matches!(
            triage_delete(PATH, false, b"HTTP 502: Bad Gateway"),
            DeleteVerdict::Failed(_)
        ));
    }

    #[test]
    fn a_bare_404_needs_a_probe() {
        assert!(matches!(
            triage_delete(PATH, false, b"gh: Not Found (HTTP 404)"),
            DeleteVerdict::Probe(_)
        ));
    }

    #[test]
    fn a_probe_that_still_finds_the_key_means_the_delete_was_refused() {
        let error = after_probe(PATH, "failure", true).unwrap_err().to_string();
        assert!(error.contains("still on the account"), "{error}");
        assert!(after_probe(PATH, "failure", false).is_ok());
    }
}
