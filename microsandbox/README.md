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
- an ssh public key under `~/.ssh` — `wbox ssh-proxy` authenticates you with it

## Flow

```sh
cd microsandbox/wbox
cargo run --release -- build          # bake the devstation-base snapshot (long, several GB)
cargo run --release -- create mybox   # cut a sandbox from it
ssh wbox-mybox                        # or: cargo run --release -- ssh mybox
cargo run --release -- list
cargo run --release -- destroy mybox
```

- `build [--force]` installs the runtime if missing, boots `debian:13`, runs `bootstrap.sh` (apt,
  the `dev` user, single-user Nix, `home-manager switch`, claude/opencode/rustup/mise), stops the
  sandbox and snapshots it as `devstation-base`.
- `create` and `build` open with a preflight: the payload files, the git identity, the base
  snapshot, and that `GH_TOKEN` really carries `admin:public_key` and `write:ssh_signing_key`.
  All failures are reported at once, before anything boots.
- `create <name> [--cpus N] [--memory MiB]` boots from that snapshot, bind-mounts
  `~/lima/claude` and `~/lima/agents`, injects `GH_TOKEN` as a secret, runs `provision.sh`,
  registers the sandbox's key with GitHub at `/user/keys` and `/user/ssh_signing_keys`, and writes
  a `Host wbox-<name>` block into `~/.ssh/config.d/wbox`. The root disk is not settable here: it
  belongs to the OCI rootfs source, so a sandbox inherits the size baked into the snapshot.
- `destroy <name>` deletes both GitHub registrations, removes the sandbox and the ssh block. It
  never starts the sandbox, so it works on a stopped one.
- `ssh-proxy <name>` is internal: the `ProxyCommand` in the generated ssh config. There is no sshd
  in the guest and no listening socket on the host — `wbox` is itself the SSH server, translating
  channels into agent execs, and writes nothing to stdout but the SSH stream.

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
