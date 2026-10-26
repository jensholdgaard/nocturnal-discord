#!/usr/bin/env bash
# Does the VM still run what deploy/ says? Read-only. Run from anywhere:
#   deploy/drift-check.sh          one line per file that differs; exit 1 if any do
#   deploy/drift-check.sh --diff   the same, plus a unified diff of each (live -> repo)
#
# Added 2026-10-26, after a check found the live nocturnal.yaml carrying archive, compaction,
# provision and roster sections the repo lacked, and the unit's credentials living in drop-ins
# the repo had never seen: install-vm.sh would have installed the repo copies over them. Every
# change made on the VM belongs in this directory the same day; this script is how to tell.
#
# The file list is explicit (no globs on the VM side), so nothing outside it - the env file, the
# encrypted credentials - is ever read. One ssh connection: ufw allows 6 new ones per 30 s.
set -uo pipefail
VM_IP="${VM_IP:-2.28.18.70}"
KEY="${DEPLOY_KEY:-$HOME/everquest-observability/.local/deploy_key}"
KNOWN="${DEPLOY_KNOWN:-$HOME/everquest-observability/.local/known}"
SHOW_DIFF=0; [ "${1:-}" = "--diff" ] && SHOW_DIFF=1

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

# repo path -> live path
declare -A MAP=(
  [deploy/nocturnal.yaml]=/etc/nocturnal/nocturnal.yaml
  [deploy/raid-bosses.yaml]=/etc/nocturnal/raid-bosses.yaml
  [deploy/nocturnal.service]=/etc/systemd/system/nocturnal.service
  [deploy/nocturnal-deploy.service]=/etc/systemd/system/nocturnal-deploy.service
  [deploy/nocturnal-deploy.timer]=/etc/systemd/system/nocturnal-deploy.timer
  [deploy/nocturnal-backup.service]=/etc/systemd/system/nocturnal-backup.service
  [deploy/nocturnal-backup.timer]=/etc/systemd/system/nocturnal-backup.timer
  [deploy/nocturnal-watchdog.service]=/etc/systemd/system/nocturnal-watchdog.service
  [deploy/nocturnal-watchdog.timer]=/etc/systemd/system/nocturnal-watchdog.timer
  [deploy/pull-deploy.sh]=/usr/local/bin/nocturnal-pull-deploy
  [deploy/backup.sh]=/usr/local/bin/nocturnal-backup
  [deploy/nocturnal-watchdog.sh]=/usr/local/bin/nocturnal-watchdog.sh
  [deploy/prometheus/nocturnal-rules.yml]=/etc/prometheus/rules/nocturnal.yml
)
for f in deploy/nocturnal.service.d/*.conf; do MAP[$f]=/etc/systemd/system/nocturnal.service.d/$(basename "$f"); done
for f in deploy/perses/*.yaml; do MAP[$f]=/etc/perses/provisioning/$(basename "$f"); done

LIVE="$(mktemp -d)"; trap 'rm -rf "$LIVE"' EXIT
# The live files that exist, plus the names in the drop-in directory (a drop-in made on the VM
# and never committed is drift too), in one tar stream.
printf '%s\n' "${MAP[@]}" | ssh -i "$KEY" -o UserKnownHostsFile="$KNOWN" -o BatchMode=yes -o ConnectTimeout=10 "root@$VM_IP" \
  'mapfile -t F; ls -1 /etc/systemd/system/nocturnal.service.d/ > /tmp/.dropins 2>/dev/null
   E=(); for f in "${F[@]}"; do [ -e "$f" ] && E+=("$f"); done
   tar -cf - "${E[@]}" /tmp/.dropins 2>/dev/null; rm -f /tmp/.dropins' | tar -xf - -C "$LIVE" \
  || { echo "could not read the VM"; exit 2; }

drift=0
for repo in $(printf '%s\n' "${!MAP[@]}" | sort); do
  live="$LIVE${MAP[$repo]}"
  if [ ! -e "$live" ]; then echo "NOT ON VM  $repo -> ${MAP[$repo]}"; drift=1; continue; fi
  if ! cmp -s "$repo" "$live"; then
    echo "DIFFERS    $repo -> ${MAP[$repo]}"; drift=1
    [ "$SHOW_DIFF" = 1 ] && diff -u --label "live ${MAP[$repo]}" --label "repo $repo" "$live" "$repo" | sed 's/^/    /'
  fi
done
while read -r name; do
  [ -n "$name" ] && [ ! -e "deploy/nocturnal.service.d/$name" ] && { echo "VM ONLY    /etc/systemd/system/nocturnal.service.d/$name (not in deploy/nocturnal.service.d/)"; drift=1; }
done < "$LIVE/tmp/.dropins"

[ "$drift" = 0 ] && echo "no drift: the VM matches deploy/ (${#MAP[@]} files)"
exit "$drift"
