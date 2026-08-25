#!/usr/bin/env bash
# Creates (or completes) a devstation Lima VM and bootstraps it.
#   ./create.sh [name]   (default: sandbox01)
# Safe to re-run: each step is skipped if already done.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="${1:-sandbox01}"

log() { printf '\n\033[1;33m==> %s\033[0m\n' "$*"; }

# --- 0. prereq check -------------------------------------------------------
log "0/4 Preflight"
if [ ! -d "$HOME/lima/claude" ] || [ ! -d "$HOME/lima/agents" ]; then
	echo "Host dirs ~/lima/claude and ~/lima/agents must exist."
	echo "Run: ${SCRIPT_DIR}/../../host/host-setup.sh ${NAME}"
	exit 1
fi
# Git identity is not in the repo: it ships via a local .env (gitignored).
if [ ! -f "${SCRIPT_DIR}/.env" ]; then
	echo "Missing ${SCRIPT_DIR}/.env (git identity for the VM)."
	echo "Run: cp ${SCRIPT_DIR}/env.example ${SCRIPT_DIR}/.env  # then fill it in"
	exit 1
fi

# --- 1. start the VM -------------------------------------------------------
log "1/4 Starting VM ($NAME)"
if limactl list --format '{{.Name}}' 2>/dev/null | grep -qx "$NAME"; then
	STATUS="$(limactl list --format '{{.Name}} {{.Status}}' | awk -v n="$NAME" '$1==n{print $2}')"
	if [ "$STATUS" = "Running" ]; then
		echo "    VM $NAME already running, skipping start."
	else
		echo "    VM $NAME exists (status: $STATUS), starting."
		limactl start "$NAME"
	fi
else
	limactl start --name "$NAME" "${SCRIPT_DIR}/lima.yaml"
fi

# --- 2. copy flake + bootstrap into VM ------------------------------------
log "2/4 Copying flake + bootstrap to VM"
# Pack the payload from this template dir; unpack into /home/dev/vm-bootstrap
# (guest home is pinned in lima.yaml; a bare ~ here would expand on the HOST).
# The VM has no git repo: the path: URI in bootstrap.sh reads all files directly.
tar -C "$SCRIPT_DIR" -czf - home vm-files bootstrap.sh .env |
	limactl shell --workdir /home/dev "$NAME" -- bash -c \
		'mkdir -p ~/vm-bootstrap && tar -xzf - -C ~/vm-bootstrap --warning=no-unknown-keyword'

# --- 3. run bootstrap in VM -----------------------------------------------
log "3/4 Bootstrapping VM (nix + home-manager + claude code)"
limactl shell --workdir /home/dev "$NAME" -- bash /home/dev/vm-bootstrap/bootstrap.sh

# --- 4. reconnect hint (login shell changed to zsh) -----------------------
log "4/4 Done"
cat <<EOF

Connect:
  limactl shell --reconnect ${NAME}     # first time: picks up new zsh login shell
  ssh lima-${NAME}                      # thereafter (requires host-setup.sh Include)

Agent forward launchd label: vm-templates.lima-agent-forward.${NAME}
Install it once per VM (if not already done):
  ${SCRIPT_DIR}/../../host/host-setup.sh ${NAME}
EOF
