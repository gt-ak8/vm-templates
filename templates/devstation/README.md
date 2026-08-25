# devstation (template)

Debian 13 Lima VM with Home Manager: zsh, starship, gh, herdr, codex, pi-coding-agent, Claude Code, OpenCode, Rust (rustup), Node LTS (+npm).

The repo seeds the VM at creation, then the VM owns its config: no live link back
to the repo. To change something, edit inside the VM, or recreate it.

## Create

```sh
cp templates/devstation/env.example templates/devstation/.env   # once, fill in git identity
./host/host-setup.sh sandbox01        # once per VM name (host dirs + launchd agent)
./templates/devstation/create.sh sandbox01
limactl shell --reconnect sandbox01   # first connect after bootstrap
```

`create.sh` starts the VM, copies `home/` + `vm-files/` + `bootstrap.sh` +
`.env` into it, runs bootstrap (nix + home-manager + writable configs + claude
code installer), then prints connect instructions.

Git identity (name, email, SSH signing pubkey) is never committed: it comes
from the gitignored `.env` (template: `env.example`). `create.sh` refuses to
run without it; `bootstrap.sh` writes it into the VM as
`~/.config/git/identity.inc` + `~/.config/git/allowed_signers`, which the
Home Manager git config includes.

Agent config ships nothing: Claude settings/CLAUDE.md come from the
`~/lima/claude` mount, and `~/.codex/AGENTS.md` / `~/.pi/agent/AGENTS.md` /
`~/.config/opencode/AGENTS.md` are symlinks to `~/.agents/AGENTS.md` (host:
`~/lima/agents/AGENTS.md`, yours to create/edit).

## Change config

- In a live VM: edit files directly (herdr config etc. are plain writable copies).
- In the template: edit this dir, then `limactl delete <name>` and re-create.
- Shared across VMs (survives recreation): `~/lima/claude` (Claude auth/settings/
  CLAUDE.md) and `~/lima/agents` (AGENTS.md, installed skills), edited host-side.

## Destroy

```sh
./host/destroy.sh sandbox01
```

Removes the VM and its launchd agent forward (which would otherwise respawn
forever against the dead VM). Shared dirs `~/lima/*` are kept.

## Files

| File             | Role                                                |
| ---------------- | --------------------------------------------------- |
| `lima.yaml`      | VM spec: Debian 13, 4 CPU / 8 GiB / 100 GiB         |
| `create.sh`      | Host-side: start VM, copy payload, run bootstrap    |
| `bootstrap.sh`   | In-VM: apt deps, nix, home-manager, configs, claude |
| `home/flake.nix` | Standalone HM flake (aarch64 + x86_64)              |
| `home/home.nix`  | User env: packages, zsh, starship, git              |
| `vm-files/`      | Writable config seeds (herdr config.toml)           |
| `env.example`    | Template for `.env` (git identity, gitignored)      |
