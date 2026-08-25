# vm-templates

Lima VM templates for disposable agentic dev sandbox VMs on macOS.

## Design

- Substrate: Debian 13 + standalone Home Manager (ported from agentic-vms)
- Payload: devstation config - herdr, starship, zsh, codex, pi-coding-agent, OpenCode (ported from homelab/templates/devstation)
- Fire-and-forget: the repo seeds the VM at creation (versions pinned by flake.lock), then the VM owns its config; no dotfile linking back to the repo. Change = edit in-VM or recreate.
- Two shared mounts only: `~/lima/claude` and `~/lima/agents` - no project-tree mount; agents work inside the VM. Shared agent files (CLAUDE.md, AGENTS.md, skills) live in these mounts, not in this repo.
- SSH agent forwarding via launchd for git signing and push (host key never touches the VM)
- No Tailscale, no Doppler - VMs are local-only, credentials logged in manually per VM
- Claude Code and OpenCode installed via official installers (not nix); self-updates work, nix store is read-only

## Create a VM

Prerequisite, once per checkout: git identity for the VMs lives in a
gitignored `.env`, not in the repo.

```sh
cp templates/devstation/env.example templates/devstation/.env
# fill in name, email, signing pubkey
```

Pick a name (`mybox` below), then from the repo root:

```sh
# 1. Host prep, once per VM name. Idempotent: global parts (shared dirs,
#    ssh Include) no-op after the first run; only the launchd agent forward
#    is per-VM.
./host/host-setup.sh mybox

# 2. Create and bootstrap (first run ~10-20 min: nix + home-manager build):
./templates/devstation/create.sh mybox

# 3. Connect:
limactl shell --reconnect mybox   # first time (picks up zsh)
ssh lima-mybox                    # thereafter
```

In the VM, check everything works:

```sh
ssh-add -l        # host agent forwarded (git push/signing)
claude            # first run prompts for auth (persists in ~/lima/claude)
herdr             # multiplexer; codex and pi-coding-agent also on PATH
```

Destroy (VM + per-VM launchd agent, keeps shared dirs):

```sh
./host/destroy.sh mybox
```

All scripts are idempotent; re-run on failure.

## Templates

| Template   | Description                   |
| ---------- | ----------------------------- |
| devstation | Agentic dev sandbox (default) |
