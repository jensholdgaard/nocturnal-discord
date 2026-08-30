#!/usr/bin/env bash
# Pull-based deploy: poll the rolling `vm-deploy` release and install a new
# binary when one appears. Run by nocturnal-deploy.timer as root.
#
# The cheap poll is the .sha256 asset — a tiny CDN GET, no API, no auth, no
# rate limit worth thinking about. Only when it differs from the installed
# binary's hash does the full download happen.
#
# Failure posture: every failure leaves the running service alone. A torn
# download fails verification; a new binary that will not serve /readyz is
# rolled back to the previous one. The safe state is always "keep running
# what runs".
set -euo pipefail

REPO="jensholdgaard/nocturnal-discord"
BASE="https://github.com/${REPO}/releases/download/vm-deploy"
BIN=/usr/local/bin/nocturnal
HEALTH="http://127.0.0.1:8090/readyz"

# Cache-busted: release assets sit behind GitHub's CDN, which served the
# *previous* checksum for a minute or so after a publish. The poll then saw
# "already installed", exited quietly, and the deploy simply did not happen
# until a later tick — with nothing in the journal to say why. A throwaway
# query parameter forces a fresh object.
want=$(curl -fsSL "${BASE}/nocturnal.sha256?cb=$(date +%s)" | tr -d '[:space:]')
[ -n "$want" ] || { echo "empty sha256 from release; skipping"; exit 0; }
have=$(sha256sum "$BIN" | awk '{print $1}')
[ "$want" = "$have" ] && exit 0

echo "new build published: ${want:0:12} (running ${have:0:12})"
tmp=$(mktemp /usr/local/bin/.nocturnal.pull.XXXXXX)
trap 'rm -f "$tmp"' EXIT
curl -fsSL -o "$tmp" "${BASE}/nocturnal?cb=$(date +%s)"
got=$(sha256sum "$tmp" | awk '{print $1}')
if [ "$got" != "$want" ]; then
  # CI uploads the binary before the sum, so a poll landing mid-publish sees a
  # mismatched pair. Not an error: the next tick sees the finished release.
  echo "checksum mismatch (mid-publish?); will retry next tick"
  exit 0
fi

cp -p "$BIN" "${BIN}.prev"
install -m 0755 "$tmp" "$BIN"
systemctl restart nocturnal

# Give it a generous window: replay of the full ledger is seconds, gateway
# connect a few more. /readyz answers 200 only after both.
for _ in $(seq 1 30); do
  sleep 2
  if curl -fsS "$HEALTH" >/dev/null 2>&1; then
    echo "deployed ${want:0:12}; healthy"
    exit 0
  fi
done

echo "new binary never became ready; rolling back to ${have:0:12}" >&2
install -m 0755 "${BIN}.prev" "$BIN"
systemctl restart nocturnal
exit 1
