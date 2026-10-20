#!/usr/bin/env bash
#
# Provision the two-node WARP egress pool as rootless Podman Quadlet units.
#
# Usage:
#   scripts/setup-warp.sh              # install units, refuses to clobber containers
#   scripts/setup-warp.sh --migrate    # remove existing test-warp-* containers first
#
# Existing named volumes are preserved, so WARP registrations are not lost when
# migrating from ad-hoc `podman run` containers.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MIGRATE=0

if [[ "${1:-}" == "--migrate" ]]; then
    MIGRATE=1
elif [[ -n "${1:-}" ]]; then
    echo "Unknown argument: $1" >&2
    exit 2
fi

if ! command -v podman >/dev/null 2>&1; then
    echo "podman is required" >&2
    exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
    echo "systemctl is required" >&2
    exit 1
fi

UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/containers/systemd"
mkdir -p "$UNIT_DIR"

# Named volumes persist WARP registration across container recreation.
for volume in test-warp-1-config test-warp-2-config; do
    if ! podman volume exists "$volume"; then
        podman volume create "$volume" >/dev/null
        echo "created volume $volume"
    fi
done

for name in test-warp-1 test-warp-2; do
    if podman container exists "$name" 2>/dev/null; then
        if [[ "$MIGRATE" -eq 1 ]]; then
            podman rm -f "$name" >/dev/null
            echo "removed existing container $name"
        else
            echo "container $name already exists; re-run with --migrate to recreate it" >&2
            exit 1
        fi
    fi
done

cp "$ROOT"/deploy/quadlet/test-warp-1.container "$UNIT_DIR/"
cp "$ROOT"/deploy/quadlet/test-warp-2.container "$UNIT_DIR/"

# Rootless containers only start at boot when the user manager runs.
if command -v loginctl >/dev/null 2>&1; then
    loginctl enable-linger "$USER" 2>/dev/null || \
        echo "warning: could not enable linger; run: loginctl enable-linger $USER" >&2
fi

systemctl --user daemon-reload
systemctl --user enable --now test-warp-1.service test-warp-2.service

# Make the host WARP client let container WARP traffic reach the edge directly.
# Without these exclusions the inner tunnel is nested inside host WARP and dies
# with `sendmmsg: message too long` at MTU 1280.
if command -v warp-cli >/dev/null 2>&1; then
    add_range() {
        local range="$1"
        if warp-cli --accept-tos tunnel ip list 2>/dev/null | grep -Fq "$range"; then
            echo "host WARP already excludes $range"
        else
            warp-cli --accept-tos tunnel ip add-range "$range" >/dev/null
            echo "host WARP now excludes $range"
        fi
    }

    add_range "162.159.192.0/24"
    add_range "162.159.193.0/24"
    add_range "162.159.195.0/24"
    add_range "188.114.96.0/24"
    add_range "188.114.97.0/24"
    add_range "188.114.98.0/24"
    add_range "188.114.99.0/24"
    add_range "2606:4700:d0::/48"
    add_range "2606:4700:d1::/48"
else
    echo "warning: warp-cli not found; if host WARP is enabled, add the endpoint exclusions manually" >&2
fi

# Wait for the published ports to answer through the WARP tunnel.
for port in 40001 40002; do
    ready=0
    for _ in $(seq 1 40); do
        if curl -fsS --max-time 5 -x "http://127.0.0.1:$port" \
            https://www.cloudflare.com/cdn-cgi/trace 2>/dev/null | grep -q '^warp=on$'; then
            ready=1
            break
        fi
        sleep 1
    done
    if [[ "$ready" -eq 1 ]]; then
        echo "node $port is serving warp=on"
    else
        echo "warning: node $port did not pass the warp=on probe yet" >&2
    fi
done

echo
echo "Run: l2c doctor"
