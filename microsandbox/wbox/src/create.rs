//! `wbox create`: cut a sandbox from the base snapshot and give it an identity.

use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::{ErrorKind, Write},
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::OpenOptionsExt,
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

/// Where the `vm-files/` configs `provision.sh` seeds from land in the guest.
const VM_FILES_PATH: &str = "/opt/wbox/vm-files";

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

/// What the sandbox's `claude` mount is seeded with, out of the Lima
/// `~/lima/claude`.
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

/// What the sandbox's `agents` mount is seeded with, out of `~/lima/agents`.
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
    // Checked before anything is written: the state record and the mounts are
    // keyed by name, and the SDK would only refuse the duplicate at boot, after
    // this run had already overwritten the live sandbox's record.
    if state::load(name)?.is_some() || Sandbox::get(name).await.is_ok() {
        return Err(
            format!("a sandbox named {name} already exists; `wbox destroy {name}` first").into(),
        );
    }
    let git_name = std::env::var("GIT_USER_NAME").unwrap_or_default();
    let git_email = std::env::var("GIT_USER_EMAIL").unwrap_or_default();

    // Sandboxes get their own host dirs, not the ones the Lima VMs share: a
    // sandbox has no business reading the host's Claude credentials, its
    // conversation history or its project state. The config and the skills are
    // copied in from the Lima share at create, so the two stay in step without
    // the sandbox seeing the rest. One dir per sandbox, too: a shared one would
    // hand whatever one guest writes (a login, a session) to every later guest,
    // and the reseed at create would rewrite it under a running sandbox.
    let mounts = mount_dir(name)?;
    let claude_mount = seed_mount(&mounts, "claude", &CLAUDE_SEED)?;
    let agents_mount = seed_mount(&mounts, "agents", &AGENTS_SEED)?;
    let claude_token = std::env::var(CLAUDE_TOKEN_VAR).unwrap_or_default();
    let claude_token = claude_token.trim();
    let copilot_token = std::env::var(COPILOT_TOKEN_VAR).unwrap_or_default();
    let has_copilot = !copilot_token.trim().is_empty();
    let credentials_stub = credentials_stub(&mounts)?;
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
             have to log in, and stores what it gets in {}",
            claude_mount.display()
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
            // would then survive into a re-created sandbox of the same name.
            // An empty stub is bound over the path, readonly, so the token
            // secret above stays the only Claude credential in the VM and
            // nothing can be written back to the host.
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

    // The snapshot carries a copy of provision.sh and vm-files/ from build
    // time, but they are the per-create half of the payload and must not need
    // a 35-minute rebuild to change. They cannot be shipped as a `.patch(..)`
    // either: a snapshot-rooted sandbox rejects patches outright ("patches
    // cannot be combined with from_snapshot"), because they would have to be
    // re-baked into the snapshot's upper. So push the current files over the
    // agent's filesystem channel, once the sandbox is up.
    eprintln!("wbox: provisioning");
    let payload = runtime::payload_dir();
    sandbox
        .fs()
        .copy_from_host(payload.join("provision.sh"), PROVISION_PATH)
        .await?;
    push_vm_files(&sandbox, &payload.join("vm-files")).await?;
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

/// Push every regular file of `vm-files/` to its guest path.
///
/// The agent channel copies single files, so the directory is walked here.
/// `/opt/wbox/vm-files` itself exists from build time.
async fn push_vm_files(sandbox: &Sandbox, dir: &Path) -> Res<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file = entry.file_name();
        let file = file.to_string_lossy();
        sandbox
            .fs()
            .copy_from_host(entry.path(), &format!("{VM_FILES_PATH}/{file}"))
            .await?;
    }
    Ok(())
}

/// Read `microsandbox/.env` into this process's environment.
///
/// Hand-rolled `KEY=VALUE` (see `parse_env`): the file holds a token, so it is
/// parsed here rather than by shelling out to anything that could echo it.
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
    for (key, value) in parse_env(&contents) {
        // SAFETY: called from `main` before the tokio runtime is built, so the
        // process is still single-threaded and no thread can be in `getenv`.
        unsafe {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

/// The `KEY=VALUE` pairs of a `.env` file, in file order.
///
/// Blank lines and lines starting with `#` are skipped, and so is a line with
/// no `=`. The key and the value are trimmed, and one layer of matching
/// quotes is taken off the value. The value runs to the end of the line: `=`
/// inside it is kept, and there are no inline comments, so a `#` is part of
/// the value.
pub fn parse_env(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            (key.trim().to_string(), value.to_string())
        })
        .collect()
}

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

/// An empty credentials file, kept in the sandbox's mount dir, bound over the
/// real one.
///
/// Rewritten every create: a stub someone edited into holding a token would
/// silently undo the shadowing it exists for.
fn credentials_stub(mounts: &Path) -> Res<PathBuf> {
    let path = mounts.join("claude-credentials-stub.json");
    std::fs::write(&path, b"{}")?;
    Ok(path)
}

/// A path under the host's `$HOME`.
fn host_path(relative: &str) -> Res<PathBuf> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home).join(relative))
}

/// The host dir holding everything bind-mounted into sandbox `name`.
///
/// Under the runtime data dir, next to the state files: one dir per sandbox,
/// so nothing a guest writes is visible to another, and `destroy` removes it
/// whole. The real path, not the `~/.wbox/<repo>` link the runtime home is
/// reached through: the runtime resolves a mount root following no symlink and
/// refuses one ("Not a directory").
pub fn mount_dir_path(name: &str) -> Res<PathBuf> {
    Ok(runtime::data_dir().join("wbox-mounts").join(name))
}

/// `mount_dir_path`, created.
fn mount_dir(name: &str) -> Res<PathBuf> {
    let path = mount_dir_path(name)?;
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Prepare `<mounts>/<share>`, seeding `entries` from `~/lima/<share>`.
///
/// The Lima share is the source of truth for the seeded entries: each one is
/// replaced, so a settings or skills change made host-side reaches the next
/// sandbox. Everything else in the dir is left alone.
fn seed_mount(mounts: &Path, share: &str, entries: &[&str]) -> Res<PathBuf> {
    let source = host_path(&format!("lima/{share}"))?;
    let target = mounts.join(share);
    std::fs::create_dir_all(&target)?;
    seed_into(&source, &target, entries)?;
    Ok(target)
}

/// Replace each of `entries` under `target` with a copy of the one under
/// `source`, skipping entries `source` lacks.
///
/// The source is a Lima guest's writable mount, so nothing in it is trusted to
/// be what it says: symlinks are never followed on the way in (see
/// `copy_tree`). What the copy carries, hooks and plugins included, is the
/// configuration the guest is meant to run, which is the point of seeding it.
fn seed_into(source: &Path, target: &Path, entries: &[&str]) -> Res<()> {
    for entry in entries {
        let from = source.join(entry);
        if std::fs::symlink_metadata(&from).is_err() {
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

/// Copy a file, a symlink or a directory tree to `to`, which must not exist.
///
/// Symlinks are copied as symlinks and never followed, at any depth: the
/// source is writable by a Lima guest, and a link replaced there to point at
/// `~/.ssh` must not pull host files into a sandbox mount. Permissions are
/// preserved (the seed carries `statusline-command.sh`, which has to stay
/// executable).
///
/// A directory is cloned in one call where the filesystem allows it (APFS,
/// same volume), which costs milliseconds whatever its size. Otherwise it is
/// copied file by file.
fn copy_tree(from: &Path, to: &Path) -> Res<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
    } else if meta.is_dir() {
        if clone_tree(from, to).is_ok() {
            return Ok(());
        }
        std::fs::create_dir(to)?;
        std::fs::set_permissions(to, meta.permissions())?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// Clone the directory `from` to `to` with `clonefile(2)`.
///
/// `CLONE_NOFOLLOW` keeps a symlink at `from` from being resolved; symlinks
/// inside the tree are cloned as symlinks by the call itself.
#[cfg(target_os = "macos")]
fn clone_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::{
        ffi::{CString, c_char, c_int},
        os::unix::ffi::OsStrExt,
    };
    unsafe extern "C" {
        fn clonefile(src: *const c_char, dst: *const c_char, flags: u32) -> c_int;
    }
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let src = CString::new(from.as_os_str().as_bytes())?;
    let dst = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both pointers are valid NUL-terminated strings for the duration
    // of the call, and the call touches nothing else in this process.
    if unsafe { clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn clone_tree(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from(ErrorKind::Unsupported))
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
    let ssh_dir = host_path(".ssh")?;
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
    choose_port(requested, &taken, |port| {
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
    })
}

/// `requested` if it is neither `taken` nor busy, else the first such port
/// from `SSH_PORT_BASE`.
///
/// `taken` are the ports other sandboxes hold; `free` says whether the host
/// will let us bind one.
fn choose_port(
    requested: Option<u16>,
    taken: &HashSet<u16>,
    free: impl Fn(u16) -> bool,
) -> Res<u16> {
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
    host_path(".ssh/config.d/wbox")
}

/// The keyword and patterns of an ssh config line that opens a block, if it
/// does.
///
/// ssh keywords are case-insensitive, and `Match` ends a `Host` block just as
/// another `Host` does.
fn block_header(line: &str) -> Option<(String, Vec<&str>)> {
    let mut tokens = line.split_whitespace();
    let keyword = tokens.next()?.to_ascii_lowercase();
    if keyword != "host" && keyword != "match" {
        return None;
    }
    Some((keyword, tokens.collect()))
}

/// `contents` without the `Host wbox-<name>` block, if it has one.
///
/// A block runs from its `Host` line to the next `Host` or `Match` line, so
/// indented options are dropped with it however many there are. The block is
/// matched on its patterns, so a header edited into `Host wbox-<name> alias`
/// still goes. Whether `contents` ends with a newline is preserved.
pub fn drop_ssh_block(contents: &str, name: &str) -> String {
    let target = format!("wbox-{name}");
    let mut kept = String::new();
    let mut skipping = false;
    for line in contents.lines() {
        if let Some((keyword, patterns)) = block_header(line) {
            skipping = keyword == "host" && patterns.contains(&target.as_str());
        }
        if !skipping {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    if !contents.ends_with('\n') && kept.ends_with('\n') {
        kept.pop();
    }
    kept
}

/// `contents` with the `Host wbox-<name>` block set to `block`, at the end.
///
/// An existing block for the name is dropped first: one left by an older wbox
/// carries that version's options, and the sandbox it described is gone
/// anyway. A `contents` without a final newline gets one before the block, and
/// that is the one change a create/destroy cycle leaves behind.
pub fn add_ssh_block(contents: &str, name: &str, block: &str) -> String {
    let mut out = drop_ssh_block(contents, name);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    out
}

/// The `Host wbox-<name>` block `create` writes.
///
/// No host key checking, and nothing recorded: the port is bound to the
/// host's loopback, so nothing off this machine can sit on it, and every
/// recreation under the same name (and so, likely, the same port) brings new
/// host keys that `accept-new` would trust once and then refuse. LogLevel
/// keeps the "Permanently added" line out of the stream, which tools that read
/// ssh output (herdr --remote) parse.
pub fn ssh_block(name: &str, ssh_port: u16) -> String {
    format!(
        "Host wbox-{name}\n    HostName 127.0.0.1\n    Port {ssh_port}\n    User dev\n    \
         StrictHostKeyChecking no\n    UserKnownHostsFile /dev/null\n    LogLevel ERROR\n"
    )
}

/// The file at `path`, or an empty string when there is none.
///
/// Only a missing file reads as empty. Any other failure (permissions, bytes
/// that are not UTF-8) is an error, because the caller is about to write the
/// result back and would otherwise truncate a file it could not read.
pub fn read_or_empty(path: &Path) -> Res<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("cannot read {}: {error}", path.display()).into()),
    }
}

/// Write `contents` to `path`, creating it user-only (0600) if it is new.
///
/// An existing file keeps its permissions.
pub fn write_private(path: &Path, contents: &str) -> Res<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// Write the sandbox's block into the wbox fragment and make sure
/// `~/.ssh/config` includes it.
fn write_ssh_config(name: &str, ssh_port: u16) -> Res<()> {
    let path = ssh_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = read_or_empty(&path)?;
    write_private(
        &path,
        &add_ssh_block(&existing, name, &ssh_block(name, ssh_port)),
    )?;

    let main = host_path(".ssh/config")?;
    let include = "Include config.d/wbox";
    let contents = read_or_empty(&main)?;
    if !contents.contains(include) {
        write_private(&main, &format!("{include}\n{contents}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod env_tests {
    use super::parse_env;

    #[test]
    fn parses_keys_values_quotes_and_comments() {
        let file = "# comment\n\nA=1\n B = spaced \nC=\"quoted\"\nD='single'\nE=a=b\nF=x # not a comment\nnoequals\nG=\n";
        let pairs = parse_env(file);
        let expect: Vec<(String, String)> = [
            ("A", "1"),
            ("B", "spaced"),
            ("C", "quoted"),
            ("D", "single"),
            ("E", "a=b"),
            ("F", "x # not a comment"),
            ("G", ""),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(pairs, expect);
    }

    #[test]
    fn strips_only_a_matching_pair_of_quotes() {
        assert_eq!(
            parse_env("A=\"open"),
            vec![("A".to_string(), "\"open".to_string())]
        );
        assert_eq!(
            parse_env("A=\"'x'\""),
            vec![("A".to_string(), "'x'".to_string())]
        );
    }
}

#[cfg(test)]
mod port_tests {
    use std::collections::HashSet;

    use super::{SSH_PORT_BASE, SSH_PORT_SPAN, choose_port};

    #[test]
    fn a_requested_port_must_be_neither_taken_nor_busy() {
        let taken: HashSet<u16> = [22200].into();
        assert!(choose_port(Some(22200), &taken, |_| true).is_err());
        assert!(choose_port(Some(22201), &taken, |_| false).is_err());
        assert_eq!(choose_port(Some(22201), &taken, |_| true).unwrap(), 22201);
    }

    #[test]
    fn skips_ports_held_by_stopped_sandboxes_and_busy_ones() {
        // 22200 belongs to a sandbox (its state file says so, running or not);
        // 22201 is bound by something else on the host.
        let taken: HashSet<u16> = [SSH_PORT_BASE].into();
        let port = choose_port(None, &taken, |p| p != SSH_PORT_BASE + 1).unwrap();
        assert_eq!(port, SSH_PORT_BASE + 2);
    }

    #[test]
    fn reports_exhaustion() {
        let taken: HashSet<u16> = (SSH_PORT_BASE..SSH_PORT_BASE + SSH_PORT_SPAN).collect();
        let error = choose_port(None, &taken, |_| true).unwrap_err().to_string();
        assert!(error.contains("--ssh-port"), "{error}");
    }
}

#[cfg(test)]
mod ssh_config_tests {
    use super::{add_ssh_block, drop_ssh_block, ssh_block};

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
        let no_newline = "Host other\n    User me";
        assert_eq!(drop_ssh_block(no_newline, "missing"), no_newline);
    }

    #[test]
    fn a_lowercase_host_line_ends_the_block() {
        let config = "Host wbox-a\n    User dev\nhost other\n    User me\n";
        assert_eq!(drop_ssh_block(config, "a"), "host other\n    User me\n");
    }

    #[test]
    fn a_match_line_ends_the_block() {
        let config = "Host wbox-a\n    User dev\nMatch all\n    User me\n";
        assert_eq!(drop_ssh_block(config, "a"), "Match all\n    User me\n");
    }

    #[test]
    fn a_header_with_an_alias_is_still_the_block() {
        let config = "Host wbox-a alias\n    User dev\nHost other\n    User me\n";
        assert_eq!(drop_ssh_block(config, "a"), "Host other\n    User me\n");
    }

    #[test]
    fn create_then_destroy_leaves_the_file_as_found() {
        let block = ssh_block("x", 22200);
        for original in [
            "",
            "Host other\n    User me\n",
            "Host wbox-a\n    User dev\n",
        ] {
            let created = add_ssh_block(original, "x", &block);
            assert!(created.ends_with(&block), "{created:?}");
            assert_eq!(drop_ssh_block(&created, "x"), original);
        }
    }

    #[test]
    fn create_then_destroy_adds_only_a_missing_final_newline() {
        let block = ssh_block("x", 22200);
        let created = add_ssh_block("Host other\n    User me", "x", &block);
        assert_eq!(created, format!("Host other\n    User me\n{block}"));
        assert_eq!(drop_ssh_block(&created, "x"), "Host other\n    User me\n");
    }

    #[test]
    fn recreating_replaces_the_old_block() {
        let old = add_ssh_block("Host other\n", "x", &ssh_block("x", 22200));
        let new = add_ssh_block(&old, "x", &ssh_block("x", 22205));
        assert_eq!(new, format!("Host other\n{}", ssh_block("x", 22205)));
    }
}

#[cfg(test)]
mod seed_tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("wbox-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn seeds_the_allowlist_and_nothing_else() {
        let home = scratch("seed");
        let lima = home.join("lima/claude");
        std::fs::create_dir_all(lima.join("skills/herdr")).unwrap();
        std::fs::create_dir_all(lima.join("projects")).unwrap();
        std::fs::write(lima.join("settings.json"), b"{}").unwrap();
        std::fs::write(lima.join(".credentials.json"), b"secret").unwrap();
        std::fs::write(lima.join("history.jsonl"), b"secret").unwrap();
        std::fs::write(lima.join("skills/herdr/SKILL.md"), b"skill").unwrap();
        std::fs::write(lima.join("projects/a.jsonl"), b"secret").unwrap();
        let mount = home.join("mounts/claude");
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

    #[test]
    fn symlinks_are_copied_as_links_and_never_followed() {
        let home = scratch("symlink");
        let secret = home.join("secret");
        std::fs::write(&secret, b"host private key").unwrap();
        let lima = home.join("lima/claude");
        std::fs::create_dir_all(lima.join("skills/real")).unwrap();
        std::fs::write(lima.join("skills/real/SKILL.md"), b"skill").unwrap();
        // A Lima guest swapped two seeded entries for links out of the share.
        std::os::unix::fs::symlink(&secret, lima.join("settings.json")).unwrap();
        std::os::unix::fs::symlink(&home, lima.join("hooks")).unwrap();
        // And one inside a seeded tree, the shape the official plugins use.
        std::os::unix::fs::symlink("SKILL.md", lima.join("skills/real/AGENTS.md")).unwrap();
        let mount = home.join("mounts/claude");
        std::fs::create_dir_all(&mount).unwrap();

        seed_into(&lima, &mount, &CLAUDE_SEED).unwrap();

        for link in ["settings.json", "hooks", "skills/real/AGENTS.md"] {
            assert!(
                std::fs::symlink_metadata(mount.join(link))
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "{link} should be a symlink"
            );
        }
        // The link points where the source's did; no host tree was copied in.
        assert_eq!(std::fs::read_link(mount.join("hooks")).unwrap(), home);
        assert_eq!(
            std::fs::read_link(mount.join("settings.json")).unwrap(),
            secret
        );
        assert_eq!(
            std::fs::read(mount.join("skills/real/SKILL.md")).unwrap(),
            b"skill"
        );

        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn the_file_by_file_copy_matches_the_clone() {
        let home = scratch("copy");
        let from = home.join("from");
        std::fs::create_dir_all(from.join("dir")).unwrap();
        std::fs::write(from.join("dir/file"), b"x").unwrap();
        std::os::unix::fs::symlink("file", from.join("dir/link")).unwrap();
        let mut perms = std::fs::metadata(from.join("dir/file"))
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(from.join("dir/file"), perms).unwrap();

        let to = home.join("to");
        std::fs::create_dir(&to).unwrap();
        for entry in std::fs::read_dir(&from).unwrap() {
            let entry = entry.unwrap();
            copy_tree(&entry.path(), &to.join(entry.file_name())).unwrap();
        }
        assert_eq!(std::fs::read(to.join("dir/file")).unwrap(), b"x");
        assert!(
            std::fs::symlink_metadata(to.join("dir/link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(to.join("dir/file"))
                .unwrap()
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o755);

        std::fs::remove_dir_all(&home).unwrap();
    }
}
