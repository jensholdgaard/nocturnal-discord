#!/usr/bin/env bash
# Restart the bot when /readyz has been 503 for three consecutive minutes.
# /readyz vouches for the ledger writer thread; a process that answers
# /healthz while every command fails is the failure this exists for
# (2026-09-03). Three strikes so a slow boot or a replay never trips it.
set -u
STATE=/run/nocturnal-watchdog.strikes
code=$(curl -s -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:8090/readyz || echo 000)
if [ "$code" = "200" ]; then echo 0 > "$STATE"; exit 0; fi
n=$(( $(cat "$STATE" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$STATE"
echo "readyz=$code strike $n/3"
if [ "$n" -ge 3 ]; then
  echo "restarting nocturnal: /readyz $code for $n minutes"
  echo 0 > "$STATE"
  systemctl restart nocturnal
fi
