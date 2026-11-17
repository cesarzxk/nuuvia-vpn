#!/usr/bin/env bash
# End-to-end M0 test: two network namespaces, one nuvia process in each, a veth
# pair between them and the encrypted L2 tunnel on top.
#
# Runs without sudo when unprivileged user namespaces are available.
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
    exec unshare -Urnm --map-root-user env NUVIA_TEST_NS=1 bash "$ROOT/scripts/test-m0.sh" "$@"
fi

BIN="$ROOT/target/debug/nuvia"
PROBE="$ROOT/target/debug/lanprobe"
TMP="$(mktemp -d /tmp/nuvia-m0.XXXXXX)"
SLEEP_PIDS=()
APP_PIDS=()

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
    echo "--- nuvia A ---"; cat "$TMP/a.log" 2>/dev/null || true
    echo "--- nuvia B ---"; cat "$TMP/b.log" 2>/dev/null || true
    echo "--- listener ---"; cat "$TMP/listen.log" 2>/dev/null || true
    echo "--- multicast ---"; cat "$TMP/mcast.log" 2>/dev/null || true
    echo "--- broadcast ---"; cat "$TMP/bcast.log" 2>/dev/null || true
    exit 1
}

echo "==> creating network namespaces"
unshare -n sleep 300 & SLEEP_PIDS+=("$!")
unshare -n sleep 300 & SLEEP_PIDS+=("$!")
A="${SLEEP_PIDS[0]}"
B="${SLEEP_PIDS[1]}"
sleep 0.3

ip link add va type veth peer name vb
ip link set va netns "$A"
ip link set vb netns "$B"

nsenter -t "$A" -n ip addr add 192.168.50.1/24 dev va
nsenter -t "$A" -n ip link set va up
nsenter -t "$A" -n ip link set lo up
nsenter -t "$B" -n ip addr add 192.168.50.2/24 dev vb
nsenter -t "$B" -n ip link set vb up
nsenter -t "$B" -n ip link set lo up

nsenter -t "$A" -n ping -c 1 -W 1 192.168.50.2 >/dev/null \
    || fail "veth connectivity between namespaces"
echo "    veth ok"

echo "==> generating identities"
IDENTITY_A="$TMP/a.toml"
IDENTITY_B="$TMP/b.toml"
"$BIN" --identity "$IDENTITY_A" keygen --name m0-a >/dev/null
"$BIN" --identity "$IDENTITY_B" keygen --name m0-b >/dev/null
PUB_A="$("$BIN" --identity "$IDENTITY_A" pubkey | awk '/^public_key:/ {print $2}')"
PUB_B="$("$BIN" --identity "$IDENTITY_B" pubkey | awk '/^public_key:/ {print $2}')"
echo "    A $PUB_A"
echo "    B $PUB_B"

echo "==> starting nuvia in both namespaces"
nsenter -t "$A" -n env RUST_LOG=info "$BIN" --identity "$IDENTITY_A" up \
    --room m0-test --password m0-password \
    --tap-name nuvia-vpn0 --ip 10.240.0.1/24 \
    --bind 192.168.50.1:42424 \
    --peer "$PUB_B@192.168.50.2:42424" \
    >"$TMP/a.log" 2>&1 &
APP_PIDS+=("$!")

nsenter -t "$B" -n env RUST_LOG=info "$BIN" --identity "$IDENTITY_B" up \
    --room m0-test --password m0-password \
    --tap-name nuvia-vpn0 --ip 10.240.0.2/24 \
    --bind 192.168.50.2:42424 \
    --peer "$PUB_A@192.168.50.1:42424" \
    >"$TMP/b.log" 2>&1 &
APP_PIDS+=("$!")

for _ in $(seq 1 75); do
    if grep -q "session established" "$TMP/a.log" 2>/dev/null \
        && grep -q "session established" "$TMP/b.log" 2>/dev/null; then
        break
    fi
    sleep 0.2
done
grep -q "session established" "$TMP/a.log" 2>/dev/null || fail "session A not established"
grep -q "session established" "$TMP/b.log" 2>/dev/null || fail "session B not established"
echo "    encrypted session established"

echo "==> ping across the tunnel"
nsenter -t "$A" -n ping -c 3 -W 2 10.240.0.2 || fail "ping A -> B"
nsenter -t "$B" -n ping -c 3 -W 2 10.240.0.1 || fail "ping B -> A"

echo "==> broadcast forwarding (10.240.0.255)"
nsenter -t "$A" -n "$PROBE" listen --port 42425 >"$TMP/listen.log" 2>&1 &
APP_PIDS+=("$!")
sleep 0.5
nsenter -t "$B" -n "$PROBE" send --port 42425 --target 10.240.0.255 \
    --message m0-broadcast >/dev/null
sleep 1
grep -q "m0-broadcast" "$TMP/listen.log" || fail "broadcast was not forwarded"

echo "==> discovery routes (M3a)"
nsenter -t "$A" -n ip route get 224.0.2.60 | grep -q 'dev nuvia-vpn0' \
    || fail "224.0.0.0/4 is not routed via nuvia-vpn0"
nsenter -t "$A" -n ip route get 255.255.255.255 | grep -q 'dev nuvia-vpn0' \
    || fail "255.255.255.255 is not routed via nuvia-vpn0"
echo "    multicast and limited broadcast use nuvia-vpn0"

echo "==> Minecraft multicast group (224.0.2.60:4445)"
nsenter -t "$A" -n "$PROBE" listen --port 4445 --group 224.0.2.60 >"$TMP/mcast.log" 2>&1 &
APP_PIDS+=("$!")
sleep 0.5
nsenter -t "$B" -n "$PROBE" send --port 4445 --group 224.0.2.60 \
    --message m3-multicast >/dev/null
sleep 1
grep -q "m3-multicast" "$TMP/mcast.log" || fail "multicast was not forwarded"

echo "==> limited broadcast (255.255.255.255)"
nsenter -t "$A" -n "$PROBE" listen --port 42426 >"$TMP/bcast.log" 2>&1 &
APP_PIDS+=("$!")
sleep 0.5
nsenter -t "$B" -n "$PROBE" send --port 42426 --target 255.255.255.255 \
    --message m3-broadcast >/dev/null
sleep 1
grep -q "m3-broadcast" "$TMP/bcast.log" || fail "limited broadcast was not forwarded"

echo
echo "M0/M3a OK: L2 tunnel, ARP/ICMP, broadcast, multicast and discovery routes work."
