#!/usr/bin/env bash
# Network smoke test: one relayer and one validator on loopback.
#
# Checks what `cargo test` does not cover: the QUIC connection, gossipsub
# delivery, and the key exchange triggered through the relayer's JSON-RPC
# `distribute_keys`. Both nodes run with `--local-test` (random identity), so
# no keypair file is read or written.
#
# Usage: scripts/smoke.sh [path-to-validator_node]   (default: target/debug/validator_node)
# Uses UDP ports 62749/62750 and TCP 127.0.0.1:8545. Takes about 40 seconds.
set -u
cd "$(dirname "$0")/.."
BIN="${1:-target/debug/validator_node}"
LOGDIR="target/smoke"
mkdir -p "$LOGDIR"
strip_ansi() { sed -E 's/\x1b\[[0-9;]*m//g' "$1"; }
count() { grep -c "$1" "$2"; }

RUST_LOG=info timeout 40 "$BIN" --relayer --local-test --listen-address 127.0.0.1 --port 62749 \
  > "$LOGDIR/relayer.log" 2>&1 &
sleep 3
RELAYER_ID=$(strip_ansi "$LOGDIR/relayer.log" | grep -o "local_peer_id=[A-Za-z0-9]*" | head -1 | cut -d= -f2)
if [ -z "$RELAYER_ID" ]; then
  echo "FAIL: the relayer did not report its peer ID"; strip_ansi "$LOGDIR/relayer.log" | tail -5; wait; exit 1
fi

RUST_LOG=info timeout 32 "$BIN" --validator --local-test --listen-address 127.0.0.1 --port 62750 \
  --bootstrap-address 127.0.0.1 --bootstrap-port 62749 --bootstrap-peer-id "$RELAYER_ID" --gen-msg \
  > "$LOGDIR/validator.log" 2>&1 &
sleep 12
VALIDATOR_ID=$(strip_ansi "$LOGDIR/validator.log" | grep -o "local_peer_id=[A-Za-z0-9]*" | head -1 | cut -d= -f2)
printf '{"jsonrpc":"2.0","method":"distribute_keys","params":{"peer_ids":["%s"]},"id":1}' "$VALIDATOR_ID" \
  > "$LOGDIR/rpc-body.json"
RPC_OUT=$(curl -s -m 8 -X POST -H 'Content-Type: application/json' \
  --data-binary @"$LOGDIR/rpc-body.json" http://127.0.0.1:8545)
wait

RESULTS="connections=$(count 'Connected established' "$LOGDIR/relayer.log")
messages_received=$(count 'Got message' "$LOGDIR/relayer.log")
key_request_received=$(count 'Received key distribution request' "$LOGDIR/validator.log")
key_response_received=$(count 'Relayer received response' "$LOGDIR/relayer.log")"
echo "relayer=$RELAYER_ID validator=${VALIDATOR_ID:-<none>}"
echo "rpc distribute_keys: ${RPC_OUT:-<no answer>}"
echo "$RESULTS"

STATUS=0
for line in $RESULTS; do
  if [ "${line#*=}" = "0" ]; then echo "FAIL: ${line%%=*}"; STATUS=1; fi
done
if grep -q "panicked" "$LOGDIR/relayer.log" "$LOGDIR/validator.log"; then echo "FAIL: a node panicked"; STATUS=1; fi
[ "$STATUS" = 0 ] && echo "PASS" || echo "logs: $LOGDIR"
exit "$STATUS"
