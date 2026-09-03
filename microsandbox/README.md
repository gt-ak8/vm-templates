# microsandbox devstation

A second devstation substrate: the same Debian 13 + Home Manager payload as the Lima devstation
template, booted on [microsandbox](https://crates.io/crates/microsandbox) microVMs instead of Lima. Its
own data stays under this directory: the runtime, the state records and the per-sandbox mounts live
in the gitignored `.runtime/`. Outside it, `create` reads `~/lima/claude` and `~/lima/agents` (the
seed), `~/.ssh/*.pub` and opencode's `~/.local/share/opencode/auth.json`, and writes
`~/.ssh/config.d/wbox` plus one `Include` line in `~/.ssh/config`. `~/.wbox/<repo-dir>` is a
symlink into `.runtime` (see "Where the runtime lives").

The entry point is `wbox` (work box), a small Rust binary over the `microsandbox` crate 0.6.16. It
replaces `host-setup.sh` + `create.sh` + `destroy.sh` + `lima.yaml`.

## Prerequisites

- macOS on arm64 (Apple silicon), with a working `libkrun` hypervisor path
- `cargo` (the crate builds on edition 2024) and `just`
- `nix` on the host only to inspect the flake; the guest installs its own
- `gh` on PATH; the tokens below authenticate it
- `microsandbox/.env`, copied from `env.example`: `GH_ADMIN_TOKEN`, `GH_TOKEN`, `GIT_USER_NAME`,
  `GIT_USER_EMAIL`, and optionally `CLAUDE_CODE_OAUTH_TOKEN`. Preflight fails without the first four.
  It is gitignored. The tokens are never printed, never written to the repo, and never stored in
  the durable sandbox config: each is referenced by name and read from the `wbox` process
  environment at each spawn, which is why `start` reads `.env` as well as `create` and `destroy`.
- an ssh public key under `~/.ssh` (`id_ed25519.pub`, `id_rsa.pub` or `id_ecdsa.pub`): it is the
  only credential the guest's sshd accepts, so `create` refuses to run without one

## Flow

```sh
just wbox install    # cargo build --release, then symlink into ~/.local/bin (WBOX_BIN_DIR overrides)
wbox build           # bake the devstation-base snapshot (long, several GB)
wbox create mybox    # cut a sandbox from it
ssh wbox-mybox       # or: wbox ssh mybox
wbox list
wbox stop mybox      # keeps the disk; start brings it back
wbox start mybox
wbox destroy mybox
just wbox check      # fmt-check, clippy -D warnings, tests
```

The root `justfile` imports `microsandbox/justfile` as the `wbox` module; `just wbox` lists its
recipes.

- `build [--force] [--disk GiB]` installs the runtime if missing, boots `debian:13` on a root disk
  of the given size (default 50 GiB; every sandbox inherits it), runs `bootstrap.sh` (apt,
  the `dev` user, sshd config, single-user Nix, `home-manager switch`, Claude Code + opencode, mise), stops the
  sandbox and snapshots it as `devstation-base`.
- `create` and `build` open with a preflight: the payload files, the git identity, the base
  snapshot, and that `GH_ADMIN_TOKEN` really carries `admin:ssh_signing_key`.
  All failures are reported at once, before anything boots.
- `create <name> [--cpus N] [--memory MiB] [--ssh-port P] [--metrics]` boots from that snapshot, publishes
  the guest's sshd on `127.0.0.1:P` (default: the first free port from 22200), bind-mounts the
  sandbox's own `.runtime/wbox-mounts/<name>/{claude,agents}` (seeded from the Lima shares, see
  below), injects `GH_TOKEN` and, when set, `CLAUDE_CODE_OAUTH_TOKEN` as secrets, pushes
  `provision.sh` and `vm-files/` into the guest and runs the script, registers the sandbox's key
  with GitHub at `/user/ssh_signing_keys`, and writes a `Host wbox-<name>` block into
  `~/.ssh/config.d/wbox`. A name that already has a sandbox or a state record is refused before
  anything is written. The root disk is not settable here: it belongs to the OCI rootfs source, so
  a sandbox inherits the size baked into the snapshot.
- Runtime metrics sampling is off unless `--metrics` is given. The sampler runs once a second
  and scans all guest RAM with `mincore` on a tokio worker, which blocks the runtime's I/O
  driver for 100-190 ms per sample: ssh keystrokes and agent calls stall visibly and the
  runtime burns ~15% of a core while the guest idles. Nothing in wbox reads the samples; they
  only feed `MSB_HOME=~/.wbox/vm-templates msb metrics <name>`, so pass `--metrics` when you
  need that for debugging. The setting is part of the stored spec, so it applies to `start`
  too and changing it means destroying and recreating the sandbox.
- `destroy <name>` deletes the GitHub registration, removes the sandbox, its mount dir and the ssh
  block. It never starts the sandbox, so it works on a stopped one. A failed GitHub call (no
  `GH_ADMIN_TOKEN`, network, 5xx) does not block the local teardown: the sandbox still goes, the
  state record is kept with the key id, `destroy` exits non-zero, and running it again retries the
  deletion.
- `stop <name>` stops a sandbox and keeps everything else: disk, port, GitHub key, ssh block.
  `start <name>` boots it again and relaunches sshd (the guest has no init to do it).
- `ssh <name>` is `ssh wbox-<name>` with a check that the sandbox is running. It never starts one.

The guest runs a real sshd (`openssh-server`, key-only, `dev` only, per-sandbox host keys generated
by `provision.sh`), published on the host loopback only. An earlier design tunnelled ssh through the
agent's virtio-console channel as a `ProxyCommand`; that channel stalls the byte stream every second
or so and made typing lag, so it is gone.

The generated block is a normal ssh host, so anything that takes an ssh target works against a
sandbox: `herdr --remote wbox-<name>` is the equivalent of `herdr --remote lima-<instance>`. Host
keys are deliberately not checked or recorded (`StrictHostKeyChecking no`,
`UserKnownHostsFile /dev/null`): the port is bound to `127.0.0.1`, so nothing off this machine can
sit on it, and recreating a sandbox under the same name (and usually the same port) brings new
host keys that `accept-new` would trust once and then refuse.

### Two scripts, not one

`bootstrap.sh` is baked into the snapshot and holds only what every VM shares. `provision.sh` runs
as `dev` at every `create` and holds what must be unique per VM: the ed25519 key (a key baked into
the snapshot would be shared by every sandbox), the git identity, and the writable configs.

Editing `provision.sh` or a config in `vm-files/` takes effect on the next `create`, no rebuild
needed, but not via the builder's `patch` mechanism: a snapshot-rooted sandbox rejects patches
outright (`"patches cannot be combined with from_snapshot"`, `backend/local/sandbox/create.rs:278`),
because they would have to be re-baked into the snapshot's upper. `create` therefore pushes the
current script and the `vm-files/` configs into the running guest over the agent's filesystem
channel (`sandbox.fs().copy_from_host(..)`, one file at a time) before executing it. The snapshot's
build-time copies only guarantee that `/opt/wbox` and its layout exist.

### Git auth: the token, not the key

Git in the sandbox reaches GitHub over HTTPS with `GH_TOKEN`. `home.nix` rewrites
`git@github.com:` and `ssh://git@github.com/` to `https://github.com/`, so a remote written as ssh
takes the same path, and `credential."https://github.com".helper` is `!gh auth git-credential`,
which hands the token to git. Clone, fetch, pull and push are the only operations that talk to
GitHub, and all four go through it; rebase, merge and commit are local and never authenticate.

That is deliberate, and it is what makes an SSO-enforced org workable. A classic token is
authorized for the org once through "Configure SSO" and every sandbox then inherits it. An SSH key
has to be authorized *per key* through the web UI, there is no API for it
(`/orgs/{org}/credential-authorizations` lists and revokes, it cannot create), and `create` mints a
fresh key per sandbox — so the ssh route means a manual click every time. The only programmatic
SSH alternative is an org SSH certificate authority, which needs GitHub Enterprise Cloud and an org
owner to trust a CA key held on your Mac.

Hence the token needs `workflow` alongside `repo` and `read:org`: without it a push touching
`.github/workflows/**` is rejected.

The per-sandbox ed25519 key is still generated and still registered, but as a **signing key only**
(`/user/ssh_signing_keys`). Signing produces a signature GitHub verifies against your account; it
never authenticates, so SSO has nothing to gate and there is no "Configure SSO" control on signing
keys. `GH_ADMIN_TOKEN` therefore needs `admin:ssh_signing_key` and nothing more.

The secret is injected as an environment variable, but only into processes the agent spawns. An
ssh session gets its environment from sshd, so `provision.sh` writes the placeholders (never the
tokens) to `~/.config/wbox/env`, which `~/.zshenv` sources. Without that, git prompts for a
username at the first clone.

Blast radius is unchanged by this: the same `GH_TOKEN` was already injected into every sandbox. If
anything it narrows, because the guest only ever holds the `$MSB_GH_TOKEN` placeholder that the
proxy substitutes on the way to `github.com`, whereas an ssh private key sits on the guest
filesystem and works from anywhere once copied out.

### Claude Code in the sandbox

Set `CLAUDE_CODE_OAUTH_TOKEN` in `microsandbox/.env` (from `claude setup-token`) and the guest is
authenticated without ever holding the token: it sees `sk-ant-oat01-msb-placeholder`, which the
interception proxy swaps for the real value on requests to `api.anthropic.com`.

Three things make that the *only* credential in the VM:

- The mount is not the Lima share. Each sandbox gets its own `.runtime/wbox-mounts/<name>/claude`
  and `.../agents`, and `create` copies an allowlist into them from `~/lima/claude` and
  `~/lima/agents`: `settings.json`, `CLAUDE.md`, `statusline-command.sh`, `hooks/`, `skills/`,
  `output-styles/`, `plugins/`, `AGENTS.md` (`CLAUDE_SEED` in `create.rs`). Your own
  `.credentials.json`, history, projects and sessions stay on the host side of that line. Edit the
  Lima copies and the next `create` picks the change up. Nothing a sandbox writes there is seen by
  another sandbox, and `destroy` removes the dir. The copy never follows a symlink: the share is
  writable by the Lima guests, so a link swapped in there arrives as a link, not as the host files
  it points at. On APFS a directory is cloned with `clonefile(2)`, so the `plugins/` tree (33 MB,
  thousands of files) costs milliseconds per create.
- `~/.claude/.credentials.json` is never seeded, but Claude Code writes one as soon as anything
  logs in. When `CLAUDE_CODE_OAUTH_TOKEN` is set, `create` binds an empty readonly stub over that
  path, so nothing is read from it and nothing written to it. Without the token the sandbox has no
  Claude credential at all and has to log in, and what it gets lands in that sandbox's own mount
  dir and goes with `destroy`.
- `~/.claude.json` is per-VM and not part of the mount, so a fresh sandbox has none of the flags
  that mark onboarding done, and Claude Code opens on the theme picker, the login-method screen
  and the bypass-permissions warning. `provision.sh` seeds them, only where absent.

The token itself lives only in the gitignored `microsandbox/.env` and this process's environment.
It is never written to a mount, and the guest only ever holds the placeholder.

### opencode's Copilot credential

opencode's GitHub Copilot provider has no env var and no `apiKey` setting: it is device-flow only,
and the request to support a PAT (opencode issue #12258) was closed as not planned. So there is
nothing to create for it and nothing to put in `.env`. Run `opencode auth login` **on the host**,
once, and `create` carries the result into every sandbox.

It reads `~/.local/share/opencode/auth.json`, takes the `access` value of the `github-copilot`
entry, and registers it as a secret allowed for `api.githubcopilot.com`. `provision.sh` writes the
guest's own `auth.json` holding only the `$MSB_COPILOT_TOKEN` placeholder, so opencode in the VM is
authenticated without the credential ever being on the guest filesystem.

One host, not two: opencode 1.18 sends the stored OAuth value straight through as a bearer token,
with no exchange against `api.github.com` and no refresh in the request path, which is why the
`refresh` value is never injected. `expires` is set far out for the same reason — nothing in the
guest can refresh, and the host's `auth.json` is re-read at every `create`, so a rotated credential
propagates on the next one.

Reading opencode's store rather than `.env` is deliberate: opencode owns and rotates this
credential, so a pasted copy would go stale without saying so. A host that has never run
`opencode auth login` still creates sandboxes; preflight notes it and moves on.

### Toolchains are per project, via mise

The image ships no global `rustup`, no global `node`, no global `bun`. What it does ship is `mise`,
with a config that carries no `[tools]` block: nothing is installed up front, and a repo pinning a
version in `mise.toml`, `.tool-versions`, `.nvmrc`, `.python-version`, `.java-version` or
`.go-version` gets it fetched on first use (`not_found_auto_install`). Its shims are on PATH for
non-interactive shells too, so `ssh wbox-<name> npm test` resolves the project's node.

A global toolchain in a disposable VM is one more thing to keep current for no gain. `sudo` is
passwordless in the guest, so an apt package or a compiler is one command away when something needs
one that mise does not manage.

mise comes from `https://mise.run` in `bootstrap.sh`, not from nixpkgs. nixpkgs pins it per release
channel — `nixos-26.05` stays on 2026.5.12 for the life of the channel — and the store is read-only,
so `mise self-update` cannot work and a repo whose `mise.toml` sets `min_version` above the pin
fails outright. Installed to `~/.local/bin` it updates itself. It is not version-pinned the way
opencode is: mise's install script carries the version rather than looking it up, so there is no
unauthenticated GitHub API call to be rate limited on.

The agent CLIs are `claude` and `opencode`, both from their official installers (self-updating, in
`~/.local/bin` and `~/.opencode/bin`). Both carry their own runtime, so neither depends on a system
node.

## Where the runtime lives, and the symlink

The runtime data (`msb`, `libkrunfw`, the image cache, snapshots, root disks, the interception CA,
`authorized_keys`), the wbox state files and the per-sandbox mount dirs all live under the
gitignored `microsandbox/.runtime`.

`wbox` does not hand the runtime *that* path. macOS caps `sockaddr_un.sun_path` at 104 bytes, and
every per-sandbox agent socket is derived from the runtime home plus a suffix of up to 52 bytes.
With the in-project path (71 bytes here) every candidate socket overflows and the sandbox refuses to
boot, telling you to "set MSB_HOME to a shorter directory". So `wbox` creates
`~/.wbox/<repo-dir-name>` as a symlink to `microsandbox/.runtime` (42 bytes here) and sets
`MSB_HOME` plus the SDK's `LocalBackend` home to the link. The crate never canonicalizes the home,
so the short string is what gets bound. The bytes stay in the repo; only the path the runtime sees
is short. `wbox` refuses to run if that path is still too long, and says by how much.

`MSB_HOME` is set before the tokio runtime starts: it is a process-wide environment write, and both
seams (`LocalBackend` for db/cache/sandboxes/snapshots, `resolve_home()` for the TLS CA and socket
fallbacks) must agree on it.

## Verified

On macOS 25.6 / arm64, against a live GitHub account:

- `wbox build` end to end. herdr and worktrunk come as prebuilt release binaries
  (`home/prebuilt.nix`), so `home-manager switch` compiles nothing. The source build used to fail
  because crates.io answers 403 to nix's `fetchurl` user agent, in a VM or not; that is what
  `prebuilt.nix` is for.
- `create`, `ssh`, `stop`, `start`, `destroy`, and the signing-key registration and deletion.
  `start` after `stop` brings sshd back and the ssh block still resolves.
- Guest CA trust. Secret injection turns TLS interception on for the whole sandbox, and the
  runtime's CA has to be trusted inside the guest for that to work. `git ls-remote https://github.com/...`
  and `gh api user` from inside a sandbox succeed through the proxy, so git and gh trust it. The
  `SSL_CERT_FILE` and `NODE_EXTRA_CA_CERTS` exports in `home.nix` stay as the lever for a tool that
  reads its own store.
- `create` on a name that already exists is refused before any record is written.
- The seed copy of `plugins/` (33 MB) takes a `create` from about 1.3 s of copying to a
  single `clonefile(2)` call; a whole `create` is around 3 s.
