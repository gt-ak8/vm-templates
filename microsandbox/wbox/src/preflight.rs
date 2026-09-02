//! Checks that run before anything is booted.
//!
//! Everything here is cheap and read-only. The point is that a missing token
//! scope or an absent snapshot costs a second, not a sandbox boot followed by
//! a provisioning run that then fails on the last call.
//!
//! Failures are collected rather than returned on the first one, so a broken
//! setup takes a single round trip to fix.

use std::process::Command;

use microsandbox::Snapshot;

use crate::{Res, build::BASE_SNAPSHOT, create, github, runtime};

/// Scopes the host-side admin token needs to register and remove the key.
///
/// Only the signing namespace: the sandbox key is never registered for
/// authentication. `admin:`, not `write:`: creating a signing key only needs
/// `write:ssh_signing_key`, but `destroy` deletes it and GitHub accepts that
/// call under `admin:ssh_signing_key` alone.
const ADMIN_SCOPES: [&str; 1] = ["admin:ssh_signing_key"];

/// Files `build` copies into the base image.
const BUILD_PAYLOAD: [&str; 3] = ["bootstrap.sh", "home", "vm-files"];

/// Checks for `wbox build`: only the payload it copies into the image.
pub fn build_preflight() -> Res<()> {
    let mut problems = Vec::new();
    check_payload(&BUILD_PAYLOAD, &mut problems);
    report(problems)
}

/// Checks for `wbox create`: the payload, the GitHub token, and the snapshot.
///
/// Assumes `create::load_env_file` has already run.
pub async fn create_preflight() -> Res<()> {
    let mut problems = Vec::new();
    check_payload(&["provision.sh"], &mut problems);
    check_git_identity(&mut problems);
    check_copilot();
    check_github(&mut problems);
    check_snapshot(&mut problems).await;
    report(problems)
}

fn check_payload(entries: &[&str], problems: &mut Vec<String>) {
    let payload = runtime::payload_dir();
    for entry in entries {
        let path = payload.join(entry);
        if !path.exists() {
            problems.push(format!("{} is missing", path.display()));
        }
    }
}

fn check_git_identity(problems: &mut Vec<String>) {
    for var in ["GIT_USER_NAME", "GIT_USER_EMAIL"] {
        if std::env::var(var).unwrap_or_default().trim().is_empty() {
            problems.push(format!(
                "{var} is empty in microsandbox/.env; commits in the sandbox would be \
                 unattributed and signature verification needs the address"
            ));
        }
    }
}

/// Note, never a failure, when the host has no Copilot credential to pass on.
///
/// opencode is one CLI among several in the image, and a sandbox without a
/// Copilot login is a working sandbox. Saying it here rather than at the end
/// of a create means the fix is known before the boot, not after it.
fn check_copilot() {
    if std::env::var(create::COPILOT_TOKEN_VAR)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        eprintln!(
            "wbox: note: no Copilot credential found on the host; run `opencode auth login` \
             if you want opencode authenticated in the sandbox"
        );
    }
}

/// Prove each token can do its job, and only its job, before anything boots.
fn check_github(problems: &mut Vec<String>) {
    if Command::new("gh").arg("--version").output().is_err() {
        problems.push("gh is not on PATH; wbox registers the sandbox key through it".into());
        return;
    }
    check_admin_token(problems);
    check_sandbox_token(problems);
}

/// The host token: must carry the key-admin scopes, and never leaves the Mac.
fn check_admin_token(problems: &mut Vec<String>) {
    let Some(head) = token_head(github::ADMIN_TOKEN_VAR, problems) else {
        return;
    };
    // An empty header means the same as no header: a fine-grained token.
    match header(&head, "x-oauth-scopes").filter(|scopes| !scopes.is_empty()) {
        // A classic token advertises its scopes, so the check is exact.
        Some(scopes) => {
            let granted: Vec<&str> = scopes.split(',').map(str::trim).collect();
            let missing: Vec<&str> = ADMIN_SCOPES
                .iter()
                .copied()
                .filter(|scope| !granted.contains(scope))
                .collect();
            if !missing.is_empty() {
                problems.push(format!(
                    "{} is missing the {} scope(s). Run `gh auth refresh -h github.com {}` \
                     and put the new token in microsandbox/.env",
                    github::ADMIN_TOKEN_VAR,
                    missing.join(" and "),
                    missing
                        .iter()
                        .map(|scope| format!("-s {scope}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
        }
        // A fine-grained token sends no scope header. Its write permissions
        // cannot be read off a GET, so probe what is provable and say so.
        None => {
            let endpoint = github::SIGNING_KEYS_ENDPOINT;
            let readable = github::gh_admin(&["api", endpoint])
                .and_then(|mut c| Ok(c.output()?))
                .map(|out| out.status.success())
                .unwrap_or(false);
            if !readable {
                problems.push(format!(
                    "{} cannot read {endpoint}; it needs read and write access to that key \
                     namespace",
                    github::ADMIN_TOKEN_VAR
                ));
            }
            eprintln!(
                "wbox: note: {} advertises no scopes (fine-grained token). Read access was \
                 verified, write access will only be proven when the key is registered.",
                github::ADMIN_TOKEN_VAR
            );
        }
    }
}

/// The sandbox token: must work, and must *not* be able to touch the keys.
///
/// This one is injected into the guest, so key-admin authority in it would
/// defeat the split. A broad token is a warning rather than a hard failure:
/// it is the user's account and their call, but it should never be silent.
fn check_sandbox_token(problems: &mut Vec<String>) {
    let Some(head) = token_head(github::SANDBOX_TOKEN_VAR, problems) else {
        return;
    };
    let Some(scopes) = header(&head, "x-oauth-scopes").filter(|s| !s.is_empty()) else {
        return;
    };
    let granted: Vec<&str> = scopes.split(',').map(str::trim).collect();
    let excessive: Vec<&str> = ADMIN_SCOPES
        .iter()
        .copied()
        .filter(|scope| granted.contains(scope))
        .collect();
    if !excessive.is_empty() {
        eprintln!(
            "wbox: warning: {} carries {}. That token is injected into the sandbox, so it \
             should hold only what git and the gh CLI need there (repo, read:org, workflow). \
             Keep the key-admin scopes in {}.",
            github::SANDBOX_TOKEN_VAR,
            excessive.join(" and "),
            github::ADMIN_TOKEN_VAR
        );
    }
}

/// Response head of `GET /user` as seen by the token in `var`.
///
/// `gh api -i` prints the headers ahead of the body. The token travels in the
/// environment, never in an argument.
fn token_head(var: &str, problems: &mut Vec<String>) -> Option<String> {
    let token = std::env::var(var).unwrap_or_default();
    if token.trim().is_empty() {
        problems.push(format!("{var} is not set (add it to microsandbox/.env)"));
        return None;
    }
    let output = Command::new("gh")
        .args(["api", "-i", "/user"])
        .env("GH_TOKEN", token)
        .env_remove("GITHUB_TOKEN")
        .output();
    match output {
        Ok(output) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => {
            problems.push(format!(
                "{var} is not usable: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
            None
        }
        Err(error) => {
            problems.push(format!("running gh: {error}"));
            None
        }
    }
}

async fn check_snapshot(problems: &mut Vec<String>) {
    if Snapshot::get(BASE_SNAPSHOT).await.is_err() {
        problems.push(format!(
            "the {BASE_SNAPSHOT} snapshot does not exist; run `wbox build` first"
        ));
    }
}

/// First value of `name` in an HTTP response head, lowercased comparison.
fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
}

fn report(problems: Vec<String>) -> Res<()> {
    if problems.is_empty() {
        return Ok(());
    }
    let mut message = String::from("preflight failed:");
    for problem in &problems {
        message.push_str("\n  - ");
        message.push_str(problem);
    }
    Err(message.into())
}

#[cfg(test)]
mod tests {
    use super::header;

    const RESPONSE: &str = "HTTP/2 200\r\nX-OAuth-Scopes: gist, read:org, repo\r\nServer: gh\r\n\r\n{\"x-oauth-scopes\": \"in the body\"}";

    #[test]
    fn reads_a_header_case_insensitively() {
        assert_eq!(
            header(RESPONSE, "x-oauth-scopes"),
            Some("gist, read:org, repo")
        );
    }

    #[test]
    fn stops_at_the_body() {
        // The body here also contains the key; only the head may be searched.
        assert_eq!(
            header("HTTP/2 200\r\n\r\nx-oauth-scopes: nope", "x-oauth-scopes"),
            None
        );
    }

    #[test]
    fn absent_header_is_none() {
        assert_eq!(header(RESPONSE, "x-accepted-oauth-scopes"), None);
    }
}
