#!/usr/bin/env bash
# Deploy nocturnal to the observability VM (eq-perses). Idempotent.
# Run from the repo root: deploy/install-vm.sh
# Uses: .env (DISCORD_BOT_TOKEN, PROVISION_BOT_TOKEN - only read when the VM has no
# credential yet or ROTATE_TOKENS=1), the eqobs deploy key, the static musl binary,
# and the migrated ledger in localdata/migrated.
#
# Re-running it on a live VM (2026-10-26): it first runs deploy/drift-check.sh and
# stops if the VM differs from deploy/, because everything below installs the repo
# copies over the live ones. Commit what the VM has first, or, when the difference
# is exactly what you mean to ship, run again with APPLY=1. Existing encrypted
# credentials are kept (ROTATE_TOKENS=1 re-encrypts them from .env), and env lines
# this script does not write (the archive's AWS_*) are carried over.
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

# The VM must still run what deploy/ says before deploy/ is installed over it.
if ! deploy/drift-check.sh --diff; then
  [ "${APPLY:-0}" = 1 ] || { echo "the VM differs from deploy/ (above): commit the VM's changes first, or rerun with APPLY=1 to install the repo copies over them"; exit 1; }
  echo "APPLY=1: installing deploy/ over the differences above"
fi

[ -x "$BIN" ] || { echo "build first: cargo zigbuild --release --target x86_64-unknown-linux-musl -p nocturnal"; exit 1; }
[ -d localdata/migrated ] || { echo "missing localdata/migrated (run nocturnal-migrate first)"; exit 1; }

echo "== copying artifacts =="
# One tar stream instead of a dozen scp calls: ufw rate-limits ssh to 6 new
# connections per 30 s, and a run of scp calls trips it halfway through.
STAGE="$(mktemp -d)"
cp "$BIN" "$STAGE/nocturnal.new"
cp deploy/nocturnal.yaml deploy/raid-bosses.yaml deploy/nocturnal.service deploy/pull-deploy.sh \
   deploy/nocturnal-deploy.service deploy/nocturnal-deploy.timer "$STAGE/"
cp -r deploy/nocturnal.service.d "$STAGE/"
cp deploy/perses/4*.yaml deploy/perses/5*-bot-*.yaml "$STAGE/"
cp deploy/prometheus/nocturnal-rules.yml "$STAGE/nocturnal-rules.yml"
tar -C localdata/migrated -czf "$STAGE/nocturnal-data.tgz" events wal 2>/dev/null || tar -C localdata/migrated -czf "$STAGE/nocturnal-data.tgz" events
tar -C "$STAGE" -cf - . | "${SSH[@]}" 'rm -rf /tmp/nocturnal-stage && mkdir -p /tmp/nocturnal-stage && tar -xf - -C /tmp/nocturnal-stage'
rm -rf "$STAGE"

echo "== installing on the VM =="
"${SSH[@]}" DISCORD_BOT_TOKEN="${DISCORD_BOT_TOKEN:-}" PROVISION_BOT_TOKEN="${PROVISION_BOT_TOKEN:-}" ROTATE_TOKENS="${ROTATE_TOKENS:-0}" 'bash -s' <<'REMOTE'
set -euo pipefail
T=/tmp/nocturnal-stage
mv $T/nocturnal.new /usr/local/bin/nocturnal.new

id -u nocturnal >/dev/null 2>&1 || useradd --system --home /var/lib/nocturnal --shell /usr/sbin/nologin nocturnal

# Ledger data on the host; seed from the migrated snapshot only on first install.
mkdir -p /var/lib/nocturnal
if [ ! -d /var/lib/nocturnal/events ] && [ ! -d /var/lib/nocturnal/wal ]; then
  tar -C /var/lib/nocturnal -xzf $T/nocturnal-data.tgz
  echo "seeded ledger from migrated snapshot"
else
  echo "existing ledger kept (no reseed)"
fi
chown -R nocturnal:nocturnal /var/lib/nocturnal
rm -f $T/nocturnal-data.tgz

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
install -m 0644 $T/nocturnal.yaml /etc/nocturnal/nocturnal.yaml
install -m 0644 $T/raid-bosses.yaml /etc/nocturnal/raid-bosses.yaml
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
#
# An existing credential is kept: a re-run is not a rotation, and .env need not
# hold the production token at all. ROTATE_TOKENS=1 re-encrypts from .env.
if [ ! -e /etc/nocturnal/bot_token.cred ] || [ "$ROTATE_TOKENS" = 1 ]; then
  : "${DISCORD_BOT_TOKEN:?DISCORD_BOT_TOKEN missing in .env (needed for a first install or ROTATE_TOKENS=1)}"
  printf '%s' "${DISCORD_BOT_TOKEN}" \
    | systemd-creds encrypt --name=bot_token - /etc/nocturnal/bot_token.cred
  chmod 600 /etc/nocturnal/bot_token.cred
fi

# Telemetry: the standard OpenTelemetry environment, nothing bespoke. The file
# also holds lines this script does not write - the archive's AWS_* (2026-09) -
# and those are carried over, not dropped: rewriting it bare once would have
# switched the ledger archive off without a word.
KEEP="$(grep -v -E '^(OTEL_|#|$)' /etc/nocturnal/env 2>/dev/null || true)"
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
[ -n "$KEEP" ] && printf '%s\n' "$KEEP" >> /etc/nocturnal/env
chmod 600 /etc/nocturnal/env

# Telemetry provisioning (M8): the bot serves /dpstoken and /dpsrevoke under the
# members' own bot identity, on a second gateway connection. That token gets the
# same treatment as the first — an encrypted credential, never an environment
# variable.
if [ -n "${PROVISION_BOT_TOKEN:-}" ] && { [ ! -e /etc/nocturnal/provision_bot_token.cred ] || [ "$ROTATE_TOKENS" = 1 ]; }; then
  printf '%s' "${PROVISION_BOT_TOKEN}" \
    | systemd-creds encrypt --name=provision_bot_token - /etc/nocturnal/provision_bot_token.cred
  chmod 600 /etc/nocturnal/provision_bot_token.cred
fi

# The derived files live outside the data directory and this bot is their only
# writer. An atomic replace renames a temp file over the target, which needs
# write on the *directory*, not just the file — the reason this is a group-owned
# directory and not simply a chown of tokens.txt. Sticky, so the bot cannot
# swap out anything else in there.
if [ -d /etc/eq-otel ]; then
  chown root:eqgw /etc/eq-otel
  chmod 3770 /etc/eq-otel
  id -nG nocturnal 2>/dev/null | grep -qw eqgw || usermod -a -G eqgw nocturnal
  [ -e /etc/eq-otel/tokens.txt ] && chown nocturnal:eqgw /etc/eq-otel/tokens.txt
  [ -e /etc/eq-otel/roles.yaml ] && chown nocturnal:eqgw /etc/eq-otel/roles.yaml
fi
[ -d /etc/perses/provisioning ] && chown -R nocturnal:perses /etc/perses/provisioning

# Pull-based CD: the VM polls the vm-deploy release and installs new builds
# itself; SSH is for initial setup only. See deploy/pull-deploy.sh.
install -m 0755 $T/pull-deploy.sh /usr/local/bin/nocturnal-pull-deploy
install -m 0644 $T/nocturnal-deploy.service /etc/systemd/system/nocturnal-deploy.service
install -m 0644 $T/nocturnal-deploy.timer /etc/systemd/system/nocturnal-deploy.timer
systemctl daemon-reload
systemctl enable --now nocturnal-deploy.timer

install -m 0755 /usr/local/bin/nocturnal.new /usr/local/bin/nocturnal
rm -f /usr/local/bin/nocturnal.new
install -m 0644 $T/nocturnal.service /etc/systemd/system/nocturnal.service
# The credentials and extra writable paths live in drop-ins, as they have on the
# VM since 2026-08 (deploy/nocturnal.service.d/).
mkdir -p /etc/systemd/system/nocturnal.service.d
for f in $T/nocturnal.service.d/*.conf; do
  install -m 0644 "$f" "/etc/systemd/system/nocturnal.service.d/$(basename "$f")"
done
for f in $T/[45]*-bot-*.yaml; do
  [ -e "$f" ] || continue   # unmatched glob under `set -u` would abort the deploy
  install -m 0644 "$f" "/etc/perses/provisioning/$(basename "$f")"
done
# Alerting rules (routed to the bot's Discord channel by the Alertmanager that
# everquest-observability's install-alerting.sh runs; harmless before it exists).
if [ -d /etc/prometheus/rules ]; then
  install -m 0644 $T/nocturnal-rules.yml /etc/prometheus/rules/nocturnal.yml
  systemctl kill -s HUP prometheus 2>/dev/null || true
fi
rm -rf $T

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
