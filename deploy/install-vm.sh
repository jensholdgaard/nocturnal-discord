#!/usr/bin/env bash
# Deploy nocturnal to the observability VM (eq-perses). Idempotent.
# Run from the repo root: deploy/install-vm.sh
# Uses: .env (DISCORD_BOT_TOKEN = controels-test-bot), the eqobs deploy key,
# the static musl binary, and the migrated ledger in localdata/migrated.
set -euo pipefail

VM_IP="${VM_IP:-2.28.18.70}"
KEY="${DEPLOY_KEY:-$HOME/everquest-observability/.local/deploy_key}"
KNOWN="${DEPLOY_KNOWN:-$HOME/everquest-observability/.local/known}"
SSH=(ssh -i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KNOWN" "root@$VM_IP")
SCP=(scp -i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KNOWN")

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
BIN="target/x86_64-unknown-linux-musl/release/nocturnal"

# Discord token for the bot comes from .env (never leaves this machine except
# into the VM's root-owned env file).
set -a; source ./.env; set +a
: "${DISCORD_BOT_TOKEN:?missing in .env}"

[ -x "$BIN" ] || { echo "build first: cargo zigbuild --release --target x86_64-unknown-linux-musl -p nocturnal"; exit 1; }
[ -d localdata/migrated ] || { echo "missing localdata/migrated (run nocturnal-migrate first)"; exit 1; }

echo "== copying artifacts =="
"${SCP[@]}" "$BIN" "root@$VM_IP:/usr/local/bin/nocturnal.new"
"${SCP[@]}" deploy/nocturnal.yaml "root@$VM_IP:/tmp/nocturnal.yaml"
"${SCP[@]}" deploy/nocturnal.service "root@$VM_IP:/tmp/nocturnal.service"
"${SCP[@]}" deploy/perses/4*.yaml "root@$VM_IP:/tmp/"
tar -C localdata/migrated -czf /tmp/nocturnal-data.tgz events wal 2>/dev/null || tar -C localdata/migrated -czf /tmp/nocturnal-data.tgz events
"${SCP[@]}" /tmp/nocturnal-data.tgz "root@$VM_IP:/tmp/nocturnal-data.tgz"
rm -f /tmp/nocturnal-data.tgz

echo "== installing on the VM =="
"${SSH[@]}" DISCORD_BOT_TOKEN="$DISCORD_BOT_TOKEN" 'bash -s' <<'REMOTE'
set -euo pipefail

id -u nocturnal >/dev/null 2>&1 || useradd --system --home /var/lib/nocturnal --shell /usr/sbin/nologin nocturnal

# Ledger data on the host; seed from the migrated snapshot only on first install.
mkdir -p /var/lib/nocturnal
if [ ! -d /var/lib/nocturnal/events ] && [ ! -d /var/lib/nocturnal/wal ]; then
  tar -C /var/lib/nocturnal -xzf /tmp/nocturnal-data.tgz
  echo "seeded ledger from migrated snapshot"
else
  echo "existing ledger kept (no reseed)"
fi
chown -R nocturnal:nocturnal /var/lib/nocturnal
rm -f /tmp/nocturnal-data.tgz

# OTLP ingest token: a dedicated line in the gateway's token file.
mkdir -p /etc/eq-otel
touch /etc/eq-otel/tokens.txt
if ! grep -q ' # nocturnal-bot$' /etc/eq-otel/tokens.txt; then
  NOC_TOKEN="$(head -c24 /dev/urandom | xxd -p)"
  echo "$NOC_TOKEN # nocturnal-bot" >> /etc/eq-otel/tokens.txt
  echo "added nocturnal-bot ingest token"
else
  NOC_TOKEN="$(awk '/ # nocturnal-bot$/{print $1}' /etc/eq-otel/tokens.txt)"
fi

# Config + secrets (root-owned).
mkdir -p /etc/nocturnal
install -m 0644 /tmp/nocturnal.yaml /etc/nocturnal/nocturnal.yaml
umask 077

# The Discord token goes in as an encrypted credential, not an environment
# variable: an environment is readable through /proc/<pid>/environ, is inherited
# by anything the service spawns, and lands in crash dumps. systemd decrypts a
# credential into a tmpfs directory only this unit can see, and the binary reads
# it via DISCORD_TOKEN_FILE, which Config::discord_token() already prefers.
#
# Encrypted against /var/lib/systemd/credential.secret, so it is host-bound on
# purpose: rebuild the VM and this file is scrap. Issuing a new token is the
# intended recovery, not restoring the ciphertext.
printf '%s' "${DISCORD_BOT_TOKEN}" \
  | systemd-creds encrypt --name=bot_token - /etc/nocturnal/bot_token.cred
chmod 600 /etc/nocturnal/bot_token.cred

# Telemetry: the standard OpenTelemetry environment, nothing bespoke.
cat > /etc/nocturnal/env <<EOF
OTEL_SERVICE_NAME=nocturnal
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4319
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer ${NOC_TOKEN}
# A bid storm lasts ~20s. At the SDK default of 60s the whole burst lands in a
# single sample, so rate() sees no increase and every quantile panel reads
# blank for the one event you most wanted to look at.
OTEL_METRIC_EXPORT_INTERVAL=15000
EOF
chmod 600 /etc/nocturnal/env

install -m 0755 /usr/local/bin/nocturnal.new /usr/local/bin/nocturnal
rm -f /usr/local/bin/nocturnal.new
install -m 0644 /tmp/nocturnal.service /etc/systemd/system/nocturnal.service
for f in /tmp/4*-bot-*.yaml; do
  [ -e "$f" ] || continue   # unmatched glob under `set -u` would abort the deploy
  install -m 0644 "$f" "/etc/perses/provisioning/$(basename "$f")"
done
rm -f /tmp/nocturnal.yaml /tmp/nocturnal.service /tmp/4*-bot-*.yaml

# Pre-flight, then run.
sudo -u nocturnal /usr/local/bin/nocturnal --config /etc/nocturnal/nocturnal.yaml --check
systemctl daemon-reload
systemctl enable --now nocturnal
systemctl restart nocturnal
sleep 3
systemctl --no-pager --lines=6 status nocturnal || true
curl -sf http://127.0.0.1:8090/readyz && echo " <- readyz"
# Nudge Perses to pick up the dashboard (provisioning reloads on restart).
systemctl restart perses 2>/dev/null || true
REMOTE

echo "== done =="
echo "verify: ssh in and 'journalctl -u nocturnal -f'; dashboard 'Nocturnal Bot' appears in Perses project everquest"
