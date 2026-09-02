# microsandbox devstation

A second devstation substrate: the same Debian 13 + Home Manager payload as the Lima devstation
template, booted on [microsandbox](https://crates.io/crates/microsandbox) microVMs instead of Lima. It is
self-contained — it reads and writes nothing outside this directory.

The entry point is `wbox` (work box), a small Rust binary over the `microsandbox` crate 0.6.15. It
replaces `host-setup.sh` + `create.sh` + `destroy.sh` + `lima.yaml`.

## Prerequisites

- macOS on arm64 (Apple silicon), with a working `libkrun` hypervisor path
- `cargo` (the crate builds on edition 2024)
- `nix` on the host only to inspect the flake; the guest installs its own
- `gh`, authenticated or reachable with the token below
- `microsandbox/.env`, copied from `env.example`: `GH_TOKEN`, `GIT_USER_NAME`, `GIT_USER_EMAIL`.
  It is gitignored. The token is never printed, never written to the repo, and never stored in the
  durable sandbox config: it is referenced by name and read from the `wbox` process environment at
  each spawn.
- an ssh public key under `~/.ssh` (`id_ed25519.pub`, `id_rsa.pub` or `id_ecdsa.pub`): it is the
  only credential the guest's sshd accepts, so `create` refuses to run without one

## Flow

```sh
cd microsandbox/wbox
cargo run --release -- build          # bake the devstation-base snapshot (long, several GB)
cargo run --release -- create mybox   # cut a sandbox from it
ssh wbox-mybox                        # or: cargo run --release -- ssh mybox
cargo run --release -- list
cargo run --release -- stop mybox     # keeps the disk; start brings it back
cargo run --release -- start mybox
cargo run --release -- destroy mybox
```

- `build [--force] [--disk GiB]` installs the runtime if missing, boots `debian:13` on a root disk
  of the given size (default 50 GiB; every sandbox inherits it), runs `bootstrap.sh` (apt,
  the `dev` user, sshd config, single-user Nix, `home-manager switch`, Claude Code + opencode, mise), stops the
  sandbox and snapshots it as `devstation-base`.
- `create` and `build` open with a preflight: the payload files, the git identity, the base
  snapshot, and that `GH_ADMIN_TOKEN` really carries `admin:ssh_signing_key`.
  All failures are reported at once, before anything boots.
- `create <name> [--cpus N] [--memory MiB] [--ssh-port P] [--metrics]` boots from that snapshot, publishes
  the guest's sshd on `127.0.0.1:P` (default: the first free port from 22200), bind-mounts
  `~/.wbox/claude` and `~/.wbox/agents` (sandbox-only dirs, seeded from the Lima shares — see
  below), injects `GH_TOKEN` and, when set,
  `CLAUDE_CODE_OAUTH_TOKEN` as secrets, runs `provision.sh`,
  registers the sandbox's key with GitHub at `/user/ssh_signing_keys`, and writes
  a `Host wbox-<name>` block into `~/.ssh/config.d/wbox`. The root disk is not settable here: it
  belongs to the OCI rootfs source, so a sandbox inherits the size baked into the snapshot.
- Runtime metrics sampling is off unless `--metrics` is given. The sampler runs once a second
  and scans all guest RAM with `mincore` on a tokio worker, which blocks the runtime's I/O
  driver for 100-190 ms per sample: ssh keystrokes and agent calls stall visibly and the
  runtime burns ~15% of a core while the guest idles. Nothing in wbox reads the samples; they
  only feed `MSB_HOME=~/.wbox/vm-templates msb metrics <name>`, so pass `--metrics` when you
  need that for debugging. The setting is part of the stored spec, so it applies to `start`
  too and changing it means destroying and recreating the sandbox.
- `destroy <name>` deletes the GitHub registration, removes the sandbox and the ssh block. It
  never starts the sandbox, so it works on a stopped one.
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

Editing `provision.sh` takes effect on the next `create`, no rebuild needed — but not via the
builder's `patch` mechanism: a snapshot-rooted sandbox rejects patches outright
(`"patches cannot be combined with from_snapshot"`, `backend/local/sandbox/create.rs:278`), because
they would have to be re-baked into the snapshot's upper. `create` therefore pushes the current
script into the running guest over the agent's filesystem channel
(`sandbox.fs().copy_from_host(..)`) before executing it. The snapshot's build-time copy is only a
fallback and a guarantee that `/opt/wbox` exists.

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

- The mount is not the Lima share. Sandboxes get `~/.wbox/claude` and `~/.wbox/agents`, and
  `create` copies an allowlist into them from `~/lima/claude` and `~/lima/agents` at every run:
  `settings.json`, `CLAUDE.md`, `statusline-command.sh`, `hooks/`,
  `skills/`, `output-styles/`, `plugins/`, `AGENTS.md` (`CLAUDE_SEED` in `create.rs`). Your own
  `.credentials.json`, history, projects and sessions stay on the host side of that line. Edit the
  Lima copies and the next `create` picks the change up; anything a sandbox writes into
  `~/.wbox/claude` is left alone.
- `~/.claude/.credentials.json` is never seeded, but Claude Code writes one as soon as anything
  logs in, and every later sandbox would read it. When `CLAUDE_CODE_OAUTH_TOKEN` is set, `create`
  binds an empty readonly stub over that path, so nothing is read from it and nothing written to
  it. Without the token the sandbox has no Claude credential at all and has to log in, and what it
  gets lands in `~/.wbox/claude`.
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

The runtime data — `msb`, `libkrunfw`, the image cache, snapshots, root disks, the interception CA,
`authorized_keys`, the wbox state files — all live under the gitignored `microsandbox/.runtime`.

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

## Unverified

- **Guest CA trust.** Secret injection turns TLS interception on for the whole sandbox. The runtime
  writes the interception CA host-side to `<home>/sandboxes/<name>/runtime/tls/ca.pem`, and the
  upstream docs claim it is added to the guest trust store — nothing in the vendored source proves
  where it lands inside a Debian guest. The Home Manager config exports `SSL_CERT_FILE` and
  `NODE_EXTRA_CA_CERTS` pointing at `/etc/ssl/certs/ca-certificates.crt` as the fallback lever: if
  the guest store is not updated by the runtime, `bootstrap.sh` (or a `patch`) has to drop the CA
  there and run `update-ca-certificates`. Not exercised yet — `wbox create` has not been run against
  a live GitHub account in this repo.
- **GitHub registration.** The `create`/`destroy` registration paths are written but have never been
  run against the real API here; no key has been created or deleted by this code.

## Build run

`cargo run --release -- build` was run end to end on macOS 25.6 / arm64. It failed, in the
`home-manager switch` step, for a reason outside this code. Everything before it worked:

- `debian:13` pulled and booted as `devstation-builder`
- apt prerequisites installed (deb.debian.org reachable from the guest, 10.1 MB in 2s)
- the `dev` user created with zsh and passwordless sudo
- **Nix installed single-user** (`nix-2.35.2-aarch64-linux`), flakes enabled
- `home-manager switch` evaluated the flake, fetched 102 paths (71 MiB) from `cache.nixos.org`,
  and then had to build `herdr-0.8.2` from source, which vendors its crates

The failing step is that vendoring. Verbatim:

```
error: Cannot build '/nix/store/x9nb7kb5hw7zbs8lj43kwcd6qc76w1p5-crate-serial2-0.2.34.tar.gz.drv'.
       Reason: builder failed with exit code 1.
       Last 17 log lines:
       > trying https://crates.io/api/v1/crates/serial2/0.2.34/download
       > curl: (22) The requested URL returned error: 403
       > Warning: Problem (retrying all errors). Retrying in 1 second. 3 retries left.
       > curl: (22) The requested URL returned error: 403
       ...
       > error: cannot download crate-serial2-0.2.34.tar.gz from any mirror
error: Cannot build '/nix/store/0rx0g2k0kf8jpgmhxi27jbgb0ll3l8v0-herdr-0.8.2.drv'.
error: Cannot build '/nix/store/xwnbl51gbkjdyyw1g5xi5jiyxcwmw42z-home-manager-generation.drv'.
```

Then, from `wbox` itself:

```
wbox: bootstrap.sh exited 1; the devstation-builder sandbox was left for inspection
```

**This is not a sandbox networking problem.** The same URL 403s from the host, outside any VM:

```
$ curl -sS -o /dev/null -w '%{http_code}\n' -L https://crates.io/api/v1/crates/serial2/0.2.34/download
403
$ curl -sS -o /dev/null -H 'User-Agent:' -w '%{http_code}\n' -L https://crates.io/api/v1/crates/serial2/0.2.34/download
200
```

crates.io is refusing requests whose `User-Agent` announces curl, which is what nix's `fetchurl`
sends. Any build of `herdr` from source hits it, in a VM or not, until crates.io relaxes that or
the derivation changes its user agent. So `devstation-base` does not exist yet: the snapshot step,
`Sandbox::remove` of the builder, and everything downstream (`create`, `ssh`, GitHub registration)
have not been exercised end to end.

The stopped `devstation-builder` sandbox is left behind on purpose — it is the evidence, and it
carries the completed apt/Nix layers. It is deliberately unlabelled, so it does not show up in
`wbox list` next to sandboxes you created; `wbox build --force` removes it and starts over.
