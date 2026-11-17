#!/usr/bin/env bash
# M5a end-to-end test: rendezvous server + two peers discovering each other and
# building a direct encrypted L2 tunnel. Runs without sudo when user namespaces
# are available.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ "${NUVIA_TEST_NS:-0}" != "1" ]]; then
    if ! command -v unshare >/dev/null; then
        echo "unshare not found; run this script with sudo instead" >&2
        exit 1
    fi
    export PATH="$HOME/.cargo/bin:$PATH"
    echo "==> building"
    cargo build --workspace --quiet
    exec unshare -Urnm --map-root-user env NUVIA_TEST_NS=1 \
        NUVIA_TEST_RELAY="${NUVIA_TEST_RELAY:-0}" bash "$ROOT/scripts/test-m5.sh" "$@"
fi

BIN="$ROOT/target/debug/nuvia"
SERVER_BIN="$ROOT/target/debug/nuvia-server"
TMP="$(mktemp -d /tmp/nuvia-m5.XXXXXX)"
SLEEP_PIDS=()
APP_PIDS=()
RELAY_FLAG=""
if [[ "${NUVIA_TEST_RELAY:-0}" == "1" ]]; then
    RELAY_FLAG="--relay"
fi

cleanup() {
    set +e
    for pid in "${APP_PIDS[@]:-}"; do kill "$pid" 2>/dev/null; done
    for pid in "${SLEEP_PIDS[@]:-}"; do kill "$pid" 2>/dev/null; done
    rm -rf "$TMP"
}
trap cleanup EXIT

fail() {
    echo
    echo "FAILED: $*" >&2
    echo "--- server ---"; cat "$TMP/server.log" 2>/dev/null || true
    echo "--- client A ---"; cat "$TMP/a.log" 2>/dev/null || true
    echo "--- client B ---"; cat "$TMP/b.log" 2>/dev/null || true
    exit 1
}

echo "==> bridge and namespaces"
ip link set lo up
ip link add br0 type bridge
ip addr add 192.168.51.1/24 dev br0
ip link set br0 up

unshare -n sleep 300 & SLEEP_PIDS+=("$!")
unshare -n sleep 300 & SLEEP_PIDS+=("$!")
A="${SLEEP_PIDS[0]}"
B="${SLEEP_PIDS[1]}"
sleep 0.3

ip link add va type veth peer name vb
ip link set vb netns "$A"
ip link set va master br0
ip link set va up
nsenter -t "$A" -n ip addr add 192.168.51.2/24 dev vb
nsenter -t "$A" -n ip link set vb up
nsenter -t "$A" -n ip link set lo up

ip link add vc type veth peer name vd
ip link set vd netns "$B"
ip link set vc master br0
ip link set vc up
nsenter -t "$B" -n ip addr add 192.168.51.3/24 dev vd
nsenter -t "$B" -n ip link set vd up
nsenter -t "$B" -n ip link set lo up

echo "    bridge ok"

echo "==> starting rendezvous server"
"$SERVER_BIN" serve --http-bind 0.0.0.0:4646 --udp-bind 0.0.0.0:8443 \
    >"$TMP/server.log" 2>&1 &
APP_PIDS+=("$!")
for _ in $(seq 1 30); do
    curl -sf http://127.0.0.1:4646/v1/healthz >/dev/null 2>&1 && break
    sleep 0.2
done
curl -sf http://127.0.0.1:4646/v1/healthz >/dev/null || fail "server did not start"

echo "==> creating invite"
INVITE="$("$BIN" invite --server http://192.168.51.1:4646 \
    --server-udp 192.168.51.1:8443 --room m5-test | head -1)"
echo "    invite length: ${#INVITE}"

echo "==> identities"
IDENTITY_A="$TMP/a.toml"
IDENTITY_B="$TMP/b.toml"
"$BIN" --identity "$IDENTITY_A" keygen --name m5-a >/dev/null
"$BIN" --identity "$IDENTITY_B" keygen --name m5-b >/dev/null

echo "==> clients"
nsenter -t "$A" -n env RUST_LOG=info "$BIN" --identity "$IDENTITY_A" up \
    --invite "$INVITE" --password m5-password $RELAY_FLAG \
    --tap-name nuvia-vpn0 --ip 10.240.0.1/24 \
    --bind 0.0.0.0:42424 \
    >"$TMP/a.log" 2>&1 &
APP_PIDS+=("$!")

nsenter -t "$B" -n env RUST_LOG=info "$BIN" --identity "$IDENTITY_B" up \
    --invite "$INVITE" --password m5-password $RELAY_FLAG \
    --tap-name nuvia-vpn0 --ip 10.240.0.2/24 \
    --bind 0.0.0.0:42424 \
    >"$TMP/b.log" 2>&1 &
APP_PIDS+=("$!")

for _ in $(seq 1 100); do
    if grep -q "session established" "$TMP/a.log" 2>/dev/null \
        && grep -q "session established" "$TMP/b.log" 2>/dev/null; then
        break
    fi
    sleep 0.3
done
grep -q "session established" "$TMP/a.log" 2>/dev/null || fail "session A not established"
grep -q "session established" "$TMP/b.log" 2>/dev/null || fail "session B not established"
echo "    rendezvous + direct session established"

echo "==> ping across the tunnel"
nsenter -t "$A" -n ping -c 3 -W 2 10.240.0.2 || fail "ping A -> B"
nsenter -t "$B" -n ping -c 3 -W 2 10.240.0.1 || fail "ping B -> A"

echo "==> server state"
curl -sf http://127.0.0.1:4646/v1/healthz
echo

echo
if [[ -n "$RELAY_FLAG" ]]; then
    echo "M5b OK: rendezvous + encrypted relay tunnel are working."
else
    echo "M5a OK: rendezvous discovery and direct encrypted tunnel are working."
fi
