#!/usr/bin/env bash
# Network smoke test: one relayer, one authorized validator and one outsider
# on loopback.
#
# Checks what `cargo test` does not cover: that an idle relayer answers the
# RPC at once; the QUIC connection, gossipsub delivery and the key exchange
# triggered through the relayer's JSON-RPC `distribute_keys`, for a validator
# the relayer authorized over the RPC; that the relayer denies an outsider
# (a peer it did not authorize); and that `revoke_node` cuts the validator off.
#
# Every node runs with HOME set to a temporary directory, removed on exit: the
# real home is never used. The relayer and the outsider run with
# `--local-test` (random identity, no file read or written). The validator
# runs without it, with the identity of the seed `validator_test` written to
# that temporary home.
#
# Usage: scripts/smoke.sh [path-to-validator_node]   (default: target/debug/validator_node)
# Uses UDP ports 62749/62750/62751 and TCP 127.0.0.1:8545. Takes about 20 seconds.
set -u
cd "$(dirname "$0")/.."
BIN="$(realpath "${1:-target/debug/validator_node}")"
LOGDIR="target/smoke"
mkdir -p "$LOGDIR"
NODE_HOME="$(mktemp -d)"
PIDS=""
cleanup() {
  for pid in $PIDS; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  rm -rf "$NODE_HOME"
}
trap cleanup EXIT
export HOME="$NODE_HOME"

VALIDATOR_SEED_ID="12D3KooWDgCeJ15KaPzEEhV8XJd1xoN7hYBBVLdx8tBPiDgA5Lfo"
strip_ansi() { sed -E 's/\x1b\[[0-9;]*m//g' "$1"; }
count() { strip_ansi "$2" | grep -cF -- "$1"; }
# Line of the first log line holding $1 in $2, or 0.
line_of() { strip_ansi "$2" | grep -nF -- "$1" | head -1 | cut -d: -f1 | grep . || echo 0; }
rpc() {
  printf '{"jsonrpc":"2.0","method":"%s","params":%s,"id":1}' "$1" "$2" > "$LOGDIR/rpc-body.json"
  curl -s -m 2 -X POST -H 'Content-Type: application/json' \
    --data-binary @"$LOGDIR/rpc-body.json" http://127.0.0.1:8545
}
peers() { printf '{"peer_ids":["%s"]}' "$1"; }

STATUS=0
check() { # name, condition result (0 = pass)
  if [ "$2" = 0 ]; then echo "ok   $1"; else echo "FAIL $1"; STATUS=1; fi
}
holds() { if "$@"; then echo 0; else echo 1; fi; }
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# The validator's identity, known in advance, in the temporary home.
GENERATED=$("$BIN" --new-peer-id --seed-phrase validator_test)
VALIDATOR_ID=$(echo "$GENERATED" | sed -n 's/^Peer ID : //p')
check "validator identity from the seed validator_test ($VALIDATOR_ID)" \
  "$(holds [ "$VALIDATOR_ID" = "$VALIDATOR_SEED_ID" ])"

RUST_LOG=info "$BIN" --relayer --local-test --listen-address 127.0.0.1 --port 62749 \
  > "$LOGDIR/relayer.log" 2>&1 &
PIDS="$PIDS $!"
sleep 3
RELAYER_ID=$(strip_ansi "$LOGDIR/relayer.log" | grep -o "local_peer_id=[A-Za-z0-9]*" | head -1 | cut -d= -f2)
if [ -z "$RELAYER_ID" ]; then
  echo "FAIL: the relayer did not report its peer ID"; strip_ansi "$LOGDIR/relayer.log" | tail -5; exit 1
fi

# The relayer is alone and idle: an RPC command must still be served at once.
# The relayer's own ID is a peer it is not connected to.
IDLE_OUT=$(rpc distribute_keys "$(peers "$RELAYER_ID")")
echo "rpc distribute_keys on the idle relayer: ${IDLE_OUT:-<no answer within 2 s>}"
check "the idle relayer answers distribute_keys within 2 seconds" \
  "$(holds contains "$IDLE_OUT" "Peer $RELAYER_ID not connected")"

# The administrator authorizes the validator before it starts.
AUTHORIZE_OUT=$(rpc authorize_node "$(peers "$VALIDATOR_ID")")
echo "rpc authorize_node: ${AUTHORIZE_OUT:-<no answer>}"
check "authorize_node lists the validator" \
  "$(holds contains "$AUTHORIZE_OUT" "\"peer_ids\":[\"$VALIDATOR_ID\"]")"

RUST_LOG=info "$BIN" --validator --listen-address 127.0.0.1 --port 62750 \
  --bootstrap-address 127.0.0.1 --bootstrap-port 62749 --bootstrap-peer-id "$RELAYER_ID" --gen-msg \
  > "$LOGDIR/validator.log" 2>&1 &
PIDS="$PIDS $!"
RUST_LOG=info "$BIN" --validator --local-test --listen-address 127.0.0.1 --port 62751 \
  --bootstrap-address 127.0.0.1 --bootstrap-port 62749 --bootstrap-peer-id "$RELAYER_ID" --gen-msg \
  > "$LOGDIR/outsider.log" 2>&1 &
PIDS="$PIDS $!"
sleep 13
STARTED_ID=$(strip_ansi "$LOGDIR/validator.log" | grep -o "local_peer_id=[A-Za-z0-9]*" | head -1 | cut -d= -f2)
OUTSIDER_ID=$(strip_ansi "$LOGDIR/outsider.log" | grep -o "local_peer_id=[A-Za-z0-9]*" | head -1 | cut -d= -f2)
echo "relayer=$RELAYER_ID validator=${STARTED_ID:-<none>} outsider=${OUTSIDER_ID:-<none>}"
check "the validator runs with the identity of its home" "$(holds [ "$STARTED_ID" = "$VALIDATOR_ID" ])"

KEYS_OUT=$(rpc distribute_keys "$(peers "$VALIDATOR_ID")")
echo "rpc distribute_keys to the validator: ${KEYS_OUT:-<no answer>}"
OUTSIDER_KEYS_OUT=$(rpc distribute_keys "$(peers "${OUTSIDER_ID:-none}")")
echo "rpc distribute_keys to the outsider: ${OUTSIDER_KEYS_OUT:-<no answer>}"
sleep 2

# Revocation: the validator's connection to the relayer is closed.
REVOKE_OUT=$(rpc revoke_node "$(peers "$VALIDATOR_ID")")
echo "rpc revoke_node: ${REVOKE_OUT:-<no answer>}"
sleep 2
AFTER_REVOKE_OUT=$(rpc distribute_keys "$(peers "$VALIDATOR_ID")")
echo "rpc distribute_keys after the revocation: ${AFTER_REVOKE_OUT:-<no answer>}"
LIST_OUT=$(rpc authorized_nodes '{}')
echo "rpc authorized_nodes: ${LIST_OUT:-<no answer>}"

R="$LOGDIR/relayer.log"; V="$LOGDIR/validator.log"; O="$LOGDIR/outsider.log"
echo "--- authorized validator"
check "connection" "$(holds [ "$(count "Connected established to $VALIDATOR_ID" "$R")" -gt 0 ])"
check "gossipsub messages received" "$(holds [ "$(count "Got message" "$R")" -gt 0 ])"
check "distribute_keys started" "$(holds contains "$KEYS_OUT" "Key distribution started successfully")"
check "key request received" "$(holds [ "$(count "Received key distribution request" "$V")" -gt 0 ])"
check "key response received" "$(holds [ "$(count "Relayer received response" "$R")" -gt 0 ])"
echo "--- outsider"
check "denied, and the relayer logs it" \
  "$(holds [ "$(count "Denied a connection from unauthorized peer ${OUTSIDER_ID:-none}" "$R")" -gt 0 ])"
check "never connected" "$(holds [ "$(count "Connected established to ${OUTSIDER_ID:-none}" "$R")" = 0 ])"
check "no gossipsub message from it" "$(holds [ "$(count "from peer: PeerId(\"${OUTSIDER_ID:-none}\")" "$R")" = 0 ])"
check "distribute_keys finds it not connected" "$(holds contains "$OUTSIDER_KEYS_OUT" "not connected")"
check "no key request received" "$(holds [ "$(count "Received key distribution request" "$O")" = 0 ])"
echo "--- revocation"
REVOKED_AT=$(line_of "Revoked peer $VALIDATOR_ID" "$R")
CLOSED_AT=$(strip_ansi "$R" | grep -nF "Connection closed with $VALIDATOR_ID" | cut -d: -f1 | tail -1)
closed_after_revocation() { [ "$REVOKED_AT" -gt 0 ] && [ "${CLOSED_AT:-0}" -gt "$REVOKED_AT" ]; }
check "revoke_node lists no peer" "$(holds contains "$REVOKE_OUT" '"peer_ids":[]')"
check "the validator's connection closed after the revocation" \
  "$(holds closed_after_revocation)"
check "distribute_keys finds it not connected" "$(holds contains "$AFTER_REVOKE_OUT" "Peer $VALIDATOR_ID not connected")"
check "authorized_nodes lists no peer" "$(holds contains "$LIST_OUT" '"peer_ids":[]')"
echo "--- files"
check "the validator's keypair is in the temporary home" "$(holds [ -f "$NODE_HOME/.chaincraft/keypair.key" ])"
check "the --local-test relayer stored no list" "$(holds [ ! -e "$NODE_HOME/.chaincraft/authorized_peers.json" ])"
no_panic() { ! grep -q "panicked" "$R" "$V" "$O"; }
check "no node panicked" "$(holds no_panic)"

[ "$STATUS" = 0 ] && echo "PASS" || echo "logs: $LOGDIR"
exit "$STATUS"
