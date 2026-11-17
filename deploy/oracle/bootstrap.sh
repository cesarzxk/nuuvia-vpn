#!/usr/bin/env bash
# One-time setup for an Ubuntu 24.04 Oracle Cloud instance.
# Run through deploy/oracle/publish.sh --bootstrap, or copy the repo and run:
#   sudo bash deploy/oracle/bootstrap.sh relay.example.com
set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
    echo "run as root (sudo)" >&2
    exit 1
fi

DOMAIN="${1:?usage: bootstrap.sh <domain>}"
if [[ ! "${DOMAIN}" =~ ^[a-zA-Z0-9.-]+$ ]]; then
    echo "invalid domain: ${DOMAIN}" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "==> installing packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
    curl ca-certificates gnupg apt-transport-https ufw

echo "==> installing caddy"
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
    | gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
    > /etc/apt/sources.list.d/caddy-stable.list
apt-get update
apt-get install -y caddy

echo "==> creating nuvia user and config"
id -u nuvia >/dev/null 2>&1 || \
    useradd --system --no-create-home --shell /usr/sbin/nologin nuvia
install -d -m 755 /etc/nuvia

install -m 644 "${SCRIPT_DIR}/../server.toml.example" /etc/nuvia/server.toml
sed -i "s|https://relay.example.com|https://${DOMAIN}|g" /etc/nuvia/server.toml

install -m 644 "${SCRIPT_DIR}/Caddyfile" /etc/caddy/Caddyfile
sed -i "s|relay.example.com|${DOMAIN}|g" /etc/caddy/Caddyfile

install -m 644 "${SCRIPT_DIR}/nuvia-server.service" /etc/systemd/system/nuvia-server.service
systemctl daemon-reload
systemctl enable nuvia-server

echo "==> firewall (UFW + Oracle security list)"
ufw allow 22/tcp
ufw allow 80/tcp
ufw allow 443/tcp
ufw allow 8443/udp
ufw --force enable

# Oracle's Ubuntu images ship a REJECT before the UFW chains. UFW sees zero
# packets until it is removed; SSH works because of the explicit 22 rule above.
echo "==> removing Oracle image REJECT that bypasses UFW"
iptables -D INPUT -j REJECT --reject-with icmp-host-prohibited 2>/dev/null || true
iptables -D FORWARD -j REJECT --reject-with icmp-host-prohibited 2>/dev/null || true
if [[ -f /etc/iptables/rules.v4 ]]; then
    sed -i '/^-A INPUT -j REJECT --reject-with icmp-host-prohibited$/d' /etc/iptables/rules.v4
    sed -i '/^-A FORWARD -j REJECT --reject-with icmp-host-prohibited$/d' /etc/iptables/rules.v4
fi
netfilter-persistent save >/dev/null 2>&1 || true

echo "==> starting caddy"
systemctl enable caddy
systemctl restart caddy

if [[ -x /usr/local/bin/nuvia-server ]]; then
    systemctl restart nuvia-server
    sleep 1
    curl -fsS http://127.0.0.1:4646/v1/healthz || true
    echo
else
    echo "nuvia-server binary not installed yet; publish.sh will upload it."
fi

echo
echo "Bootstrap done. Next:"
echo "  1. Open TCP 80/443 and UDP 8443 in the Oracle security list."
echo "  2. Point ${DOMAIN} to this instance's reserved public IP."
echo "  3. Run publish.sh to build and install nuvia-server."
echo "  4. curl https://${DOMAIN}/v1/healthz"
