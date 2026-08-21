#!/usr/bin/env bash
# Post-deploy verification for nocturnal on eq-perses. Read-only.
set -uo pipefail
VM_IP="${VM_IP:-2.28.18.70}"
KEY="${DEPLOY_KEY:-$HOME/everquest-observability/.local/deploy_key}"
KNOWN="${DEPLOY_KNOWN:-$HOME/everquest-observability/.local/known}"
ssh -i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$KNOWN" "root@$VM_IP" 'bash -s' <<'REMOTE'
echo "— services —"
for s in nocturnal eq-gateway prometheus jaeger perses; do
  printf "%-12s %s\n" "$s" "$(systemctl is-active $s 2>/dev/null)"
done
echo "— bot —"
curl -s http://127.0.0.1:8090/readyz; echo " <- readyz"
journalctl -u nocturnal --since "-30 min" --no-pager | grep -oE 'gateway ready[^"]*' | tail -1
echo "OTLP export errors (last 5 min): $(journalctl -u nocturnal --since "-5 min" --no-pager | grep -c ExportError)"
echo "— data in the stack —"
echo "ledger head in Prometheus: $(curl -s 'http://127.0.0.1:9090/api/v1/query?query=nocturnal_ledger_seq' | grep -oE '"[0-9]+"\]' | tail -1)"
echo "jaeger services: $(curl -s http://127.0.0.1:16686/api/services | head -c 200)"
REMOTE
