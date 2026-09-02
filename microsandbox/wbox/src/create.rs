//! `wbox create`: cut a sandbox from the base snapshot and give it an identity.

use std::{
    collections::HashSet,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
};

use microsandbox::{Sandbox, sandbox::SecretSource};

use crate::{
    Res,
    build::BASE_SNAPSHOT,
    cli::{CreateOpts, DEFAULT_CPUS, DEFAULT_MEMORY_MIB, SSH_PORT_BASE},
    github, preflight, runtime, ssh,
    state::{self, State},
};

/// How many ports above `SSH_PORT_BASE` `create` will try.
const SSH_PORT_SPAN: u16 = 100;

/// Where `provision.sh` lands in the guest.
const PROVISION_PATH: &str = "/opt/wbox/provision.sh";

/// Marker `provision.sh` prints the sandbox public key on.
const PUBKEY_MARKER: &str = "WBOX_PUBKEY ";

/// Env var Claude Code reads its OAuth token from.
const CLAUDE_TOKEN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Placeholder handed to the guest in place of `GH_TOKEN`. The SDK's default
/// shape, pinned here because `provision.sh` writes it into the login
/// environment (see `guest_env`) and must agree with the proxy.
const GH_TOKEN_PLACEHOLDER: &str = "$MSB_GH_TOKEN";

/// Host Claude Code sends that token to.
const CLAUDE_API_HOST: &str = "api.anthropic.com";

/// Placeholder handed to the guest in place of the Claude token.
///
/// Shaped like a real `claude setup-token` value on purpose. The default
/// placeholder is `$MSB_<VAR>`, and a client that sanity-checks the token
/// prefix before its first request would reject that without ever reaching
/// the proxy that would have substituted it.
const CLAUDE_TOKEN_PLACEHOLDER: &str = "sk-ant-oat01-msb-placeholder";

/// Env var the Copilot OAuth token is carried in, host-side and as a secret.
///
/// Not read from `microsandbox/.env`: `opencode auth login` owns this
/// credential and rotates it, so `create` reads whatever is current out of the
/// host's `auth.json` rather than a copy that goes stale unnoticed.
pub const COPILOT_TOKEN_VAR: &str = "COPILOT_OAUTH_TOKEN";

/// Host opencode sends the Copilot token to.
///
/// One host, not two: opencode 1.18 sends the stored OAuth value straight to
/// the Copilot API as a bearer token. There is no exchange against
/// `api.github.com` and no refresh in the request path, which is why the
/// `refresh` value is not injected at all.
const COPILOT_API_HOST: &str = "api.githubcopilot.com";

/// Placeholder handed to the guest in place of the Copilot token.
const COPILOT_TOKEN_PLACEHOLDER: &str = "$MSB_COPILOT_TOKEN";

/// opencode's credential store, on the host and in the guest alike.
const OPENCODE_AUTH: &str = ".local/share/opencode/auth.json";

/// The provider key `opencode auth login` writes the Copilot entry under.
const COPILOT_PROVIDER: &str = "github-copilot";

/// Claude Code's credentials file, inside the `~/.claude` mount.
const CLAUDE_CREDENTIALS: &str = "/home/dev/.claude/.credentials.json";

/// What `~/.wbox/claude` is seeded with, out of the Lima `~/lima/claude`.
///
/// An allowlist, not a list of exclusions: the rest of that directory is
/// session state (`history.jsonl`, `projects/`, `sessions/`) or a credential,
/// and anything Claude Code starts writing there in a future version has to be
/// opted in rather than land in a sandbox by default. `.credentials.json` is
/// the one that matters, and it is not here.
///
/// `hooks/` and `statusline-command.sh` are in because `settings.json` names
/// them; a sandbox seeded without them starts with a broken hook. `plugins/`
/// is in because the plugins `settings.json` enables carry skills of their own,
/// and without it a fresh sandbox has to re-clone every marketplace before it
/// has them.
const CLAUDE_SEED: [&str; 7] = [
    "settings.json",
    "CLAUDE.md",
    "statusline-command.sh",
    "hooks",
    "skills",
    "output-styles",
    "plugins",
];

/// What `~/.wbox/agents` is seeded with, out of `~/lima/agents`.
const AGENTS_SEED: [&str; 1] = ["AGENTS.md"];

/// `wbox create <name> [--cpus N] [--memory MiB] [--ssh-port P]`.
pub async fn create(name: &str, opts: CreateOpts) -> Res<()> {
    runtime::ensure_runtime().await?;
    // `microsandbox/.env` was already read into the environment by `main`,
    // before the tokio runtime existed: `set_var` is only sound while the
    // process is single-threaded (any other thread in a `getenv` races it).
    // preflight proves GH_TOKEN exists, carries both key scopes, and that the
    // base snapshot is there, so nothing below boots a sandbox it will have to
    // throw away.
    preflight::create_preflight().await?;
    let git_name = std::env::var("GIT_USER_NAME").unwrap_or_default();
    let git_email = std::env::var("GIT_USER_EMAIL").unwrap_or_default();

    // Sandboxes get their own host dirs, not the ones the Lima VMs share: a
    // sandbox has no business reading the host's Claude credentials, its
    // conversation history or its project state. The config and the skills are
    // copied in from the Lima share at every create, so the two stay in step
    // without the sandbox seeing the rest.
    let claude_mount = seed_mount("claude", &CLAUDE_SEED)?;
    let agents_mount = seed_mount("agents", &AGENTS_SEED)?;
    let claude_token = std::env::var(CLAUDE_TOKEN_VAR).unwrap_or_default();
    let claude_token = claude_token.trim();
    let copilot_token = std::env::var(COPILOT_TOKEN_VAR).unwrap_or_default();
    let has_copilot = !copilot_token.trim().is_empty();
    let credentials_stub = credentials_stub()?;
    let authorized_keys = host_public_keys()?;
    let ssh_port = pick_ssh_port(opts.ssh_port)?;
    // Recorded before the boot: a create that fails past this point leaves a
    // sandbox behind, and its port must stay off the table for the next one
    // until `destroy` clears both.
    state::save(&State {
        name: name.to_string(),
        signing_key_id: None,
        ssh_port: Some(ssh_port),
    })?;

    eprintln!("wbox: creating {name} from {BASE_SNAPSHOT}, sshd on 127.0.0.1:{ssh_port}");
    let builder = Sandbox::builder(name)
        .from_snapshot(BASE_SNAPSHOT)
        .cpus(opts.cpus.unwrap_or(DEFAULT_CPUS))
        .memory(opts.memory.unwrap_or(DEFAULT_MEMORY_MIB));
    // The runtime's once-a-second metrics sampler scans all guest RAM with
    // `mincore` on a tokio worker, which blocks the I/O driver for 100-190 ms
    // each time: every ssh keystroke and agent call feels laggy. Only `msb
    // metrics` reads the samples, so keep them off unless asked for.
    let builder = if opts.metrics {
        builder
    } else {
        builder.disable_metrics_sample()
    };
    let sandbox = builder
        // Loopback only: the guest's sshd is reachable from this machine and
        // nowhere else, whatever the guest itself may accept.
        .port_bind(Ipv4Addr::LOCALHOST.into(), ssh_port, 22)
        .hostname(name)
        .user("dev")
        .workdir("/home/dev")
        .shell("/usr/bin/zsh")
        .label(runtime::WBOX_LABEL.0, runtime::WBOX_LABEL.1)
        .volume("/home/dev/.claude", |m| m.bind(&claude_mount))
        .volume("/home/dev/.agents", |m| m.bind(&agents_mount))
        // The value is read from this process's environment at each spawn and
        // never persisted: the guest only ever sees the `$MSB_GH_TOKEN`
        // placeholder, which the proxy substitutes for the allowed hosts.
        .secret(|s| {
            s.env(github::SANDBOX_TOKEN_VAR)
                .source(SecretSource::Env {
                    var: github::SANDBOX_TOKEN_VAR.into(),
                })
                .placeholder(GH_TOKEN_PLACEHOLDER)
                .allow_host("api.github.com")
                .allow_host("github.com")
                .allow_host_pattern("*.githubusercontent.com")
        });

    // Optional: without it the sandbox falls back to the credentials in the
    // bind-mounted ~/.claude, which is the pre-token behaviour.
    let sandbox = if claude_token.is_empty() {
        eprintln!(
            "wbox: note: {CLAUDE_TOKEN_VAR} is not set; Claude Code in the sandbox will \
             have to log in, and stores what it gets in ~/.wbox/claude"
        );
        sandbox
    } else {
        sandbox
            .secret(|s| {
                s.env(CLAUDE_TOKEN_VAR)
                    .source(SecretSource::Env {
                        var: CLAUDE_TOKEN_VAR.into(),
                    })
                    .placeholder(CLAUDE_TOKEN_PLACEHOLDER)
                    .allow_host(CLAUDE_API_HOST)
            })
            // The mount is never seeded with credentials, but Claude Code
            // writes its own there the moment anything logs in, and that file
            // would then be read by every later sandbox. An empty stub is
            // bound over the path, readonly, so the token secret above stays
            // the only Claude credential in the VM and nothing can be written
            // back to the host.
            .volume(CLAUDE_CREDENTIALS, |m| m.bind(&credentials_stub).readonly())
    };

    // opencode's Copilot provider. Same shape as the Claude token: the guest's
    // auth.json holds the placeholder that provision.sh writes, and the proxy
    // substitutes the real value on the way to the Copilot API. Optional, and
    // absent it simply leaves opencode unauthenticated.
    let sandbox = if has_copilot {
        sandbox.secret(|s| {
            s.env(COPILOT_TOKEN_VAR)
                .source(SecretSource::Env {
                    var: COPILOT_TOKEN_VAR.into(),
                })
                .placeholder(COPILOT_TOKEN_PLACEHOLDER)
                .allow_host(COPILOT_API_HOST)
        })
    } else {
        eprintln!(
            "wbox: note: no {COPILOT_PROVIDER} entry in ~/{OPENCODE_AUTH}; opencode in the \
             sandbox will have to log in. Run `opencode auth login` on the host and re-create."
        );
        sandbox
    };

    let sandbox = sandbox.create_detached().await?;

    // The snapshot carries a copy of provision.sh from build time, but the
    // script is the per-create half of the payload and must not need a
    // 35-minute rebuild to change. It cannot be shipped as a `.patch(..)`
    // either: a snapshot-rooted sandbox rejects patches outright ("patches
    // cannot be combined with from_snapshot"), because they would have to be
    // re-baked into the snapshot's upper. So push the current script over the
    // agent's filesystem channel, once the sandbox is up.
    eprintln!("wbox: provisioning");
    sandbox
        .fs()
        .copy_from_host(runtime::payload_dir().join("provision.sh"), PROVISION_PATH)
        .await?;
    let output = sandbox
        .exec_with("/bin/bash", |e| {
            e.args([PROVISION_PATH])
                .user("dev")
                .cwd("/home/dev")
                .env("HOME", "/home/dev")
                .env("GIT_USER_NAME", &git_name)
                .env("GIT_USER_EMAIL", &git_email)
                .env("WBOX_AUTHORIZED_KEYS", &authorized_keys)
                .env("WBOX_GUEST_ENV", guest_env(!claude_token.is_empty()))
                // The placeholder, never the token: provision.sh writes this
                // verbatim into the guest's auth.json.
                .env(
                    "WBOX_COPILOT_PLACEHOLDER",
                    if has_copilot {
                        COPILOT_TOKEN_PLACEHOLDER
                    } else {
                        ""
                    },
                )
        })
        .await?;
    let stdout = output.stdout()?;
    eprint!("{}", output.stderr()?);
    if !output.status().success {
        return Err(format!("provision.sh exited {}", output.status().code).into());
    }
    let pubkey = stdout
        .lines()
        .find_map(|line| line.strip_prefix(PUBKEY_MARKER))
        .ok_or("provision.sh printed no public key")?
        .trim()
        .to_string();
    ssh::ensure_sshd(&sandbox).await?;

    // Signing only. The key never authenticates to GitHub, so an SSO-enforced
    // org has nothing to authorize on it; git uses GH_TOKEN over HTTPS instead.
    let title = format!("wbox-{name}");
    eprintln!("wbox: registering the sandbox signing key with GitHub as {title}");
    let signing_key_id = github::register_key(github::SIGNING_KEYS_ENDPOINT, &title, &pubkey)?;

    state::save(&State {
        name: name.to_string(),
        signing_key_id: Some(signing_key_id),
        ssh_port: Some(ssh_port),
    })?;

    write_ssh_config(name, ssh_port)?;
    println!("sandbox {name} ready: ssh wbox-{name} (127.0.0.1:{ssh_port})");
    Ok(())
}

/// Read `microsandbox/.env` into this process's environment.
///
/// Hand-rolled `KEY=VALUE`: the file holds a token, so it is parsed here
/// rather than by shelling out to anything that could echo it.
///
/// The Copilot token is picked up here too, from opencode's own store rather
/// than the file, for the same single-threaded reason.
///
/// **Call this from `main`, before the tokio runtime is built.** It writes the
/// process environment, which is unsound once more than one thread is running.
pub fn load_env_file() -> Res<()> {
    load_copilot_token();
    let path = runtime::payload_dir().join(".env");
    let contents = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read {}: {error} (copy microsandbox/env.example to it)",
            path.display()
        )
    })?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        // SAFETY: called from `main` before the tokio runtime is built, so the
        // process is still single-threaded and no thread can be in `getenv`.
        unsafe {
            std::env::set_var(key.trim(), value);
        }
    }
    Ok(())
}

/// Resolve (and create) a host directory under `$HOME`.
/// Read the Copilot OAuth token out of the host's opencode store into
/// `COPILOT_TOKEN_VAR`.
///
/// Silent when there is nothing to read: opencode is one CLI among several in
/// the image, and a machine that has never run `opencode auth login` should
/// still create sandboxes without a word about it. `create` says so once, at
/// the point where it would have injected the secret.
///
/// **Call this from `main`, before the tokio runtime is built.** Same
/// soundness constraint as `load_env_file`.
fn load_copilot_token() {
    let Ok(path) = host_path(OPENCODE_AUTH) else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(auth) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(token) = auth
        .get(COPILOT_PROVIDER)
        .and_then(|entry| entry.get("access"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return;
    };
    // SAFETY: called from `load_env_file`, itself called from `main` before
    // the tokio runtime is built, so the process is still single-threaded.
    unsafe {
        std::env::set_var(COPILOT_TOKEN_VAR, token);
    }
}

/// An empty credentials file, kept on the host, bound over the real one.
///
/// Rewritten every create: a stub someone edited into holding a token would
/// silently undo the shadowing it exists for.
fn credentials_stub() -> Res<PathBuf> {
    let path = runtime::home()?.join("claude-credentials-stub.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, b"{}")?;
    Ok(path)
}

fn host_dir(relative: &str) -> Res<PathBuf> {
    let path = host_path(relative)?;
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn host_path(relative: &str) -> Res<PathBuf> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home).join(relative))
}

/// Prepare `~/.wbox/<name>`, refreshing `entries` from `~/lima/<name>`.
///
/// The Lima share is the source of truth for the seeded entries: each one is
/// replaced on every create, so a settings or skills change made host-side
/// reaches the next sandbox. Everything else in the wbox dir is left alone,
/// which is where a sandbox's own writes live.
fn seed_mount(name: &str, entries: &[&str]) -> Res<PathBuf> {
    let source = host_path(&format!("lima/{name}"))?;
    let target = host_dir(&format!(".wbox/{name}"))?;
    seed_into(&source, &target, entries)?;
    Ok(target)
}

fn seed_into(source: &Path, target: &Path, entries: &[&str]) -> Res<()> {
    for entry in entries {
        let from = source.join(entry);
        if !from.exists() {
            continue;
        }
        let to = target.join(entry);
        remove_any(&to)?;
        copy_tree(&from, &to)?;
    }
    Ok(())
}

fn remove_any(path: &Path) -> Res<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(_) => {}
    }
    Ok(())
}

/// Copy a file or a directory tree, preserving permissions (the seed carries
/// `statusline-command.sh`, which has to stay executable).
fn copy_tree(from: &Path, to: &Path) -> Res<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// `KEY=value` lines `provision.sh` installs into the login environment.
///
/// Secrets reach the guest as environment variables, but only in processes the
/// agent spawns. An ssh session gets its environment from sshd, so the
/// placeholders have to be written down for the shell to export. Placeholders
/// only: the proxy swaps them for the real values on the way out, so a file
/// holding them gives away nothing.
fn guest_env(with_claude: bool) -> String {
    let mut lines = vec![format!(
        "{}={GH_TOKEN_PLACEHOLDER}",
        github::SANDBOX_TOKEN_VAR
    )];
    if with_claude {
        lines.push(format!("{CLAUDE_TOKEN_VAR}={CLAUDE_TOKEN_PLACEHOLDER}"));
    }
    lines.join("\n")
}

/// The host's ssh public keys, one per line, for the guest's `authorized_keys`.
///
/// The guest's sshd is key-only, so without one there is no way in at all.
fn host_public_keys() -> Res<String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let ssh_dir = Path::new(&home).join(".ssh");
    let mut keys = Vec::new();
    for file in ["id_ed25519.pub", "id_rsa.pub", "id_ecdsa.pub"] {
        if let Ok(key) = std::fs::read_to_string(ssh_dir.join(file)) {
            keys.push(key.trim().to_string());
        }
    }
    if keys.is_empty() {
        return Err(format!(
            "no ssh public key under {}; run `ssh-keygen -t ed25519` first",
            ssh_dir.display()
        )
        .into());
    }
    Ok(keys.join("\n"))
}

/// The host port the guest's sshd is published on.
///
/// Ports held by other sandboxes, stopped ones included, come from the state
/// files. A bind probe rules out whatever else on the host holds a port.
fn pick_ssh_port(requested: Option<u16>) -> Res<u16> {
    let taken: HashSet<u16> = state::all()?
        .into_iter()
        .filter_map(|s| s.ssh_port)
        .collect();
    let free = |port: u16| TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    if let Some(port) = requested {
        if taken.contains(&port) {
            return Err(format!("port {port} is already held by another sandbox").into());
        }
        if !free(port) {
            return Err(format!("port {port} is in use on the host").into());
        }
        return Ok(port);
    }
    (SSH_PORT_BASE..SSH_PORT_BASE + SSH_PORT_SPAN)
        .find(|port| !taken.contains(port) && free(*port))
        .ok_or_else(|| {
            format!(
                "no free port in {SSH_PORT_BASE}..{}; pass --ssh-port",
                SSH_PORT_BASE + SSH_PORT_SPAN
            )
            .into()
        })
}

/// The ssh config fragment wbox owns.
pub fn ssh_config_path() -> Res<PathBuf> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home).join(".ssh").join("config.d").join("wbox"))
}

/// Append a `Host wbox-<name>` block, adding the `Include` line if needed.
/// `contents` without the `Host wbox-<name>` block, if it has one.
///
/// A block runs from its `Host` line to the next one, so indented options are
/// dropped with it however many there are.
pub fn drop_ssh_block(contents: &str, name: &str) -> String {
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
    kept
}

fn write_ssh_config(name: &str, ssh_port: u16) -> Res<()> {
    let path = ssh_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // No blank line around the block: `destroy` removes exactly the lines
    // written here, so repeated create/destroy cycles leave the file as they
    // found it instead of growing a blank line each time.
    // No host key checking, and nothing recorded: the port is bound to the
    // host's loopback, so nothing off this machine can sit on it, and every
    // recreation under the same name (and so, likely, the same port) brings
    // new host keys that `accept-new` would trust once and then refuse.
    // LogLevel keeps the "Permanently added" line out of the stream, which
    // tools that read ssh output (herdr --remote) parse.
    let block = format!(
        "Host wbox-{name}\n    HostName 127.0.0.1\n    Port {ssh_port}\n    User dev\n    \
         StrictHostKeyChecking no\n    UserKnownHostsFile /dev/null\n    LogLevel ERROR\n"
    );
    // Rewritten rather than skipped when present: a block left by an older
    // wbox carries that version's options, and the sandbox it described is
    // gone anyway.
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, format!("{}{block}", drop_ssh_block(&existing, name)))?;

    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let main = Path::new(&home).join(".ssh").join("config");
    let include = "Include config.d/wbox";
    let contents = std::fs::read_to_string(&main).unwrap_or_default();
    if !contents.contains(include) {
        std::fs::write(&main, format!("{include}\n{contents}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::drop_ssh_block;

    const CONFIG: &str = "Host wbox-a\n    User dev\n    LogLevel ERROR\nHost wbox-ab\n    User dev\nHost other\n    User me\n";

    #[test]
    fn drops_the_block_and_all_its_options() {
        assert_eq!(
            drop_ssh_block(CONFIG, "a"),
            "Host wbox-ab\n    User dev\nHost other\n    User me\n"
        );
    }

    #[test]
    fn matches_the_whole_host_name() {
        // "wbox-a" must not take "wbox-ab" with it.
        assert!(drop_ssh_block(CONFIG, "ab").contains("Host wbox-a\n"));
        assert!(!drop_ssh_block(CONFIG, "ab").contains("Host wbox-ab\n"));
    }

    #[test]
    fn leaves_a_config_without_the_block_alone() {
        assert_eq!(drop_ssh_block(CONFIG, "missing"), CONFIG);
    }
}

#[cfg(test)]
mod seed_tests {
    use super::*;

    #[test]
    fn seeds_the_allowlist_and_nothing_else() {
        let home = std::env::temp_dir().join(format!("wbox-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let lima = home.join("lima/claude");
        std::fs::create_dir_all(lima.join("skills/herdr")).unwrap();
        std::fs::create_dir_all(lima.join("projects")).unwrap();
        std::fs::write(lima.join("settings.json"), b"{}").unwrap();
        std::fs::write(lima.join(".credentials.json"), b"secret").unwrap();
        std::fs::write(lima.join("history.jsonl"), b"secret").unwrap();
        std::fs::write(lima.join("skills/herdr/SKILL.md"), b"skill").unwrap();
        std::fs::write(lima.join("projects/a.jsonl"), b"secret").unwrap();
        let mount = home.join(".wbox/claude");
        std::fs::create_dir_all(&mount).unwrap();
        seed_into(&lima, &mount, &CLAUDE_SEED).unwrap();
        assert!(mount.join("settings.json").is_file());
        assert!(mount.join("skills/herdr/SKILL.md").is_file());
        assert!(!mount.join(".credentials.json").exists());
        assert!(!mount.join("history.jsonl").exists());
        assert!(!mount.join("projects").exists());

        // A stale seeded entry is replaced, a sandbox's own file is kept.
        std::fs::write(mount.join("settings.json"), b"stale").unwrap();
        std::fs::write(mount.join("history.jsonl"), b"guest").unwrap();
        seed_into(&lima, &mount, &CLAUDE_SEED).unwrap();
        assert_eq!(std::fs::read(mount.join("settings.json")).unwrap(), b"{}");
        assert!(mount.join("history.jsonl").is_file());

        std::fs::remove_dir_all(&home).unwrap();
    }
}
