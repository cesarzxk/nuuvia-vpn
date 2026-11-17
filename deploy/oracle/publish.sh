#!/usr/bin/env bash
# Uploads the source tree to the VPS, builds nuvia-server there and installs it.
#
# Required:
#   VPS_HOST=203.0.113.10
#   DOMAIN=relay.example.com          (only with --bootstrap)
#
# Optional:
#   VPS_USER=ubuntu                   (default: ubuntu)
#   SSH_KEY=~/.ssh/nuvia/ssh-key-2026-11-16.key
#
# First deploy:
#   VPS_HOST=... DOMAIN=relay.example.com ./deploy/oracle/publish.sh --bootstrap
#
# Updates:
#   VPS_HOST=... ./deploy/oracle/publish.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VPS_HOST="${VPS_HOST:?export VPS_HOST with the public IP or hostname}"
VPS_USER="${VPS_USER:-ubuntu}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/nuvia/ssh-key-2026-11-16.key}"
BOOTSTRAP=0

if [[ "${1:-}" == "--bootstrap" ]]; then
    BOOTSTRAP=1
fi
if [[ "${BOOTSTRAP}" == "1" ]]; then
    DOMAIN="${DOMAIN:?export DOMAIN with the DNS name}"
fi

if [[ ! -f "${SSH_KEY}" ]]; then
    echo "SSH key not found: ${SSH_KEY}" >&2
    exit 1
fi

BUILD="${BUILD:-local}"   # local = static x86_64 musl built here; remote = build on VPS

SSH=(ssh -i "${SSH_KEY}" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new)
TARGET="${VPS_USER}@${VPS_HOST}"
REMOTE_DIR="/tmp/nuvia-src"

echo "==> uploading source to ${TARGET}:${REMOTE_DIR}"
tar -C "${ROOT}" --exclude=target --exclude=dist --exclude=.git -czf - . \
    | "${SSH[@]}" "${TARGET}" \
        "rm -rf ${REMOTE_DIR} && mkdir -p ${REMOTE_DIR} && tar -xzf - -C ${REMOTE_DIR}"

if [[ "${BOOTSTRAP}" == "1" ]]; then
    echo "==> bootstrap (Caddy, systemd, firewall)"
    "${SSH[@]}" "${TARGET}" \
        "sudo bash ${REMOTE_DIR}/deploy/oracle/bootstrap.sh '${DOMAIN}'"
fi

if [[ "${BUILD}" == "local" ]]; then
    echo "==> building static x86_64 binary locally"
    export PATH="$HOME/.cargo/bin:$PATH"
    cargo build --release -p nuvia-server --target x86_64-unknown-linux-musl

    REMOTE_ARCH="$("${SSH[@]}" "${TARGET}" 'uname -m')"
    if [[ "${REMOTE_ARCH}" != "x86_64" ]]; then
        echo "VPS is ${REMOTE_ARCH}; use BUILD=remote instead" >&2
        exit 1
    fi

    echo "==> uploading static binary"
    "${SSH[@]}" "${TARGET}" \
        'cat > /tmp/nuvia-server && chmod +x /tmp/nuvia-server' \
        < "${ROOT}/target/x86_64-unknown-linux-musl/release/nuvia-server"
    REMOTE_BINARY="/tmp/nuvia-server"
else
    echo "==> building nuvia-server on the VPS"
    "${SSH[@]}" "${TARGET}" 'set -e
if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
export PATH="$HOME/.cargo/bin:$PATH"
cd /tmp/nuvia-src
cargo build --release -p nuvia-server'
    REMOTE_BINARY="${REMOTE_DIR}/target/release/nuvia-server"
fi

echo "==> installing and restarting"
"${SSH[@]}" "${TARGET}" "set -e
sudo install -m 755 ${REMOTE_BINARY} /usr/local/bin/nuvia-server
sudo systemctl enable nuvia-server >/dev/null 2>&1 || true
sudo systemctl restart nuvia-server
sleep 1
curl -fsS http://127.0.0.1:4646/v1/healthz"
echo
echo "Deploy done."
