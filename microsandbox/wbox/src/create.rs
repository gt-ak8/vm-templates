//! `wbox create`: cut a sandbox from the base snapshot and give it an identity.

use std::path::{Path, PathBuf};

use microsandbox::{Sandbox, sandbox::SecretSource};

use crate::{
    Res,
    build::BASE_SNAPSHOT,
    cli::CreateOpts,
    github, preflight, runtime,
    state::{self, State},
};

/// Where `provision.sh` lands in the guest.
const PROVISION_PATH: &str = "/opt/wbox/provision.sh";

/// Marker `provision.sh` prints the sandbox public key on.
const PUBKEY_MARKER: &str = "WBOX_PUBKEY ";

/// Env var Claude Code reads its OAuth token from.
const CLAUDE_TOKEN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Host Claude Code sends that token to.
const CLAUDE_API_HOST: &str = "api.anthropic.com";

/// Placeholder handed to the guest in place of the Claude token.
///
/// Shaped like a real `claude setup-token` value on purpose. The default
/// placeholder is `$MSB_<VAR>`, and a client that sanity-checks the token
/// prefix before its first request would reject that without ever reaching
/// the proxy that would have substituted it.
const CLAUDE_TOKEN_PLACEHOLDER: &str = "sk-ant-oat01-msb-placeholder";

/// Claude Code's credentials file, inside the shared `~/.claude` mount.
const CLAUDE_CREDENTIALS: &str = "/home/dev/.claude/.credentials.json";

/// Default resources when `--cpus/--memory` are not given. The root disk is
/// not among them: it is a property of the OCI rootfs source, so a
/// snapshot-rooted sandbox inherits the size baked in at build time and the
/// SDK rejects setting it here.
const DEFAULT_CPUS: u8 = 4;
const DEFAULT_MEMORY_MIB: u32 = 8192;

/// `wbox create <name> [--cpus N] [--memory MiB]`.
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

    // Both host dirs are the ones the Lima templates already share, so config
    // written by any VM is seen by all of them.
    let claude_mount = host_dir("lima/claude")?;
    let agents_mount = host_dir("lima/agents")?;
    let claude_token = std::env::var(CLAUDE_TOKEN_VAR).unwrap_or_default();
    let claude_token = claude_token.trim();
    let credentials_stub = credentials_stub()?;
    authorize_host_key(name)?;

    eprintln!("wbox: creating {name} from {BASE_SNAPSHOT}");
    let sandbox = Sandbox::builder(name)
        .from_snapshot(BASE_SNAPSHOT)
        .cpus(opts.cpus.unwrap_or(DEFAULT_CPUS))
        .memory(opts.memory.unwrap_or(DEFAULT_MEMORY_MIB))
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
                .allow_host("api.github.com")
                .allow_host("github.com")
                .allow_host_pattern("*.githubusercontent.com")
        });

    // Optional: without it the sandbox falls back to the credentials in the
    // bind-mounted ~/.claude, which is the pre-token behaviour.
    let sandbox = if claude_token.is_empty() {
        eprintln!(
            "wbox: note: {CLAUDE_TOKEN_VAR} is not set; Claude Code will use whatever \
             credentials the mounted ~/.claude holds"
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
            // The shared ~/.claude holds the host's own OAuth tokens, and a
            // sandbox has no business reading them: the whole point of the
            // secret above is that the guest only ever sees a placeholder.
            // The file cannot be deleted host-side (the Lima VMs authenticate
            // with it), so an empty stub is bound over it. Readonly, so the
            // guest cannot write credentials back into the shared dir either.
            .volume(CLAUDE_CREDENTIALS, |m| {
                m.bind(&credentials_stub).readonly()
            })
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

    let title = format!("wbox-{name}");
    eprintln!("wbox: registering the sandbox key with GitHub as {title}");
    let auth_key_id = github::register_key(github::AUTH_KEYS_ENDPOINT, &title, &pubkey)?;
    let signing_key_id = github::register_key(github::SIGNING_KEYS_ENDPOINT, &title, &pubkey)?;

    state::save(&State {
        name: name.to_string(),
        auth_key_id: Some(auth_key_id),
        signing_key_id: Some(signing_key_id),
    })?;

    write_ssh_config(name)?;
    println!("sandbox {name} ready: ssh wbox-{name}");
    Ok(())
}

/// Read `microsandbox/.env` into this process's environment.
///
/// Hand-rolled `KEY=VALUE`: the file holds a token, so it is parsed here
/// rather than by shelling out to anything that could echo it.
///
/// **Call this from `main`, before the tokio runtime is built.** It writes the
/// process environment, which is unsound once more than one thread is running.
pub fn load_env_file() -> Res<()> {
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
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let path = Path::new(&home).join(relative);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Make sure the host's ssh public keys can authenticate to `wbox ssh-proxy`.
///
/// The proxy is an in-process SSH server; it accepts the keys listed in
/// `<runtime home>/ssh/authorized_keys`.
fn authorize_host_key(name: &str) -> Res<()> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let ssh_dir = Path::new(&home).join(".ssh");
    let mut keys = Vec::new();
    for name in ["id_ed25519.pub", "id_rsa.pub", "id_ecdsa.pub"] {
        if let Ok(key) = std::fs::read_to_string(ssh_dir.join(name)) {
            keys.push(key.trim().to_string());
        }
    }
    if keys.is_empty() {
        // Only the ProxyCommand route needs it. `wbox ssh <name>` attaches
        // in-process with a key pair it generates per connection, so the
        // sandbox is still perfectly usable without one.
        eprintln!(
            "wbox: warning: no ssh public key under {} — `ssh wbox-{name}` and VS Code \
             Remote-SSH will not authenticate. `wbox ssh {name}` works regardless; \
             re-run `wbox create` after `ssh-keygen -t ed25519` to fix it.",
            ssh_dir.display()
        );
        return Ok(());
    }
    let authorized = runtime::home()?.join("ssh").join("authorized_keys");
    if let Some(parent) = authorized.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&authorized).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
    for key in keys {
        if !lines.iter().any(|line| line.trim() == key) {
            lines.push(key);
        }
    }
    std::fs::write(&authorized, format!("{}\n", lines.join("\n")))?;
    Ok(())
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

fn write_ssh_config(name: &str) -> Res<()> {
    let path = ssh_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let exe = std::env::current_exe()?;
    // No blank line around the block: `destroy` removes exactly the lines
    // written here, so repeated create/destroy cycles leave the file as they
    // found it instead of growing a blank line each time.
    // No host key checking, and nothing recorded: a sandbox is reached only
    // through the ProxyCommand below, an in-process SSH server with no network
    // path to spoof, and every recreation of the same name brings a new key.
    // `accept-new` would trust the first one and then refuse the sandbox after
    // the next `create`. LogLevel keeps the "Permanently added" line out of
    // the stream, which tools that read ssh output (herdr --remote) parse.
    let block = format!(
        "Host wbox-{name}\n    User dev\n    ProxyCommand {} ssh-proxy {name}\n    \
         StrictHostKeyChecking no\n    UserKnownHostsFile /dev/null\n    LogLevel ERROR\n",
        exe.display()
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
