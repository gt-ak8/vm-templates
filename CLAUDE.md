# vm-templates

Lima VM templates for disposable agentic dev sandbox VMs on macOS.

- `templates/` - one dir per VM template (lima.yaml + bootstrap + HM flake)
- `host/` - host-side scripts: shared dir setup, launchd agent for ssh-agent forwarding, destroy
- Create a VM: `templates/devstation/create.sh [name]` (after `host/host-setup.sh [name]`)
- Destroy a VM: `host/destroy.sh [name]` (VM + launchd agent; keeps shared dirs)

## Template register

| Template   | Info                    |
| ---------- | ----------------------- |
| devstation | `templates/devstation/` |
