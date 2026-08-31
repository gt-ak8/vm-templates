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

use crate::{Res, build::BASE_SNAPSHOT, github, runtime};

/// Scopes a classic token needs to register and remove the sandbox key.
///
/// `admin:` on both, not `write:`: creating a signing key only needs
/// `write:ssh_signing_key`, but `destroy` deletes it and GitHub accepts that
/// call under `admin:ssh_signing_key` alone.
const REQUIRED_SCOPES: [&str; 2] = ["admin:public_key", "admin:ssh_signing_key"];

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

/// Prove the token can manage both key namespaces before a sandbox is booted.
fn check_github(problems: &mut Vec<String>) {
    if Command::new("gh").arg("--version").output().is_err() {
        problems.push("gh is not on PATH; wbox registers the sandbox key through it".into());
        return;
    }
    if std::env::var("GH_TOKEN")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        problems.push("GH_TOKEN is not set (add it to microsandbox/.env)".into());
        return;
    }

    // `gh api -i` prints the response headers ahead of the body. The token is
    // never echoed: it travels in the environment, not in the arguments.
    let output = match Command::new("gh").args(["api", "-i", "/user"]).output() {
        Ok(output) => output,
        Err(error) => {
            problems.push(format!("running gh: {error}"));
            return;
        }
    };
    if !output.status.success() {
        problems.push(format!(
            "GH_TOKEN is not usable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        return;
    }

    let body = String::from_utf8_lossy(&output.stdout);
    // An empty header means the same as no header: a fine-grained token.
    match header(&body, "x-oauth-scopes").filter(|scopes| !scopes.is_empty()) {
        // A classic token advertises its scopes, so the check is exact.
        Some(scopes) => {
            let granted: Vec<&str> = scopes.split(',').map(str::trim).collect();
            let missing: Vec<&str> = REQUIRED_SCOPES
                .iter()
                .copied()
                .filter(|scope| !granted.contains(scope))
                .collect();
            if !missing.is_empty() {
                problems.push(format!(
                    "GH_TOKEN is missing the {} scope(s). Run `gh auth refresh -h github.com {}` \
                     and put the new token in microsandbox/.env",
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
            for endpoint in [github::AUTH_KEYS_ENDPOINT, github::SIGNING_KEYS_ENDPOINT] {
                let probe = Command::new("gh").args(["api", endpoint]).output();
                let readable = probe.map(|out| out.status.success()).unwrap_or(false);
                if !readable {
                    problems.push(format!(
                        "GH_TOKEN cannot read {endpoint}; it needs read and write access to \
                         that key namespace"
                    ));
                }
            }
            eprintln!(
                "wbox: note: GH_TOKEN advertises no scopes (fine-grained token). Read access \
                 was verified, write access will only be proven when the key is registered."
            );
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
