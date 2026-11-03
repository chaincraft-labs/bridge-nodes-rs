#!/usr/bin/env bash
# Network smoke test: one relayer, two authorized validators and two
# outsiders on loopback.
#
# Checks what `cargo test` does not cover: that an idle relayer answers the
# RPC at once; the QUIC connection, gossipsub delivery and the key exchange
# triggered through the relayer's JSON-RPC `distribute_keys`, for a validator
# the relayer authorized over the RPC; that each validator accepts the signed
# tree of authorized peers; that the two validators find each other through
# Kademlia via the relayer; that the relayer denies an outsider (a peer it did
# not authorized), and so does a validator an outsider dials directly; that
# `revoke_node` on the second validator makes the first close its connection
# to it; and that `revoke_node` cuts the first validator off the relayer.
#
# Every node runs with HOME set to a temporary directory, removed on exit: the
# real home is never used. The relayer and the outsiders run with
# `--local-test` (random identity, no file read or written). Each validator
# runs without it, in its own temporary home, with the identity of a seed
# written there: `validator_test` and `validator_test_2`.
#
# Usage: scripts/smoke.sh [path-to-validator_node]   (default: target/debug/validator_node)
# Uses UDP ports 62749 (relayer), 62750 and 62752 (validators), 62751 and
# 62753 (outsiders), and TCP 127.0.0.1:8545. Takes about 40 seconds.
set -u
cd "$(dirname "$0")/.."
BIN="$(realpath "${1:-target/debug/validator_node}")"
LOGDIR="target/smoke"
mkdir -p "$LOGDIR"
NODE_HOME="$(mktemp -d)"
VALIDATOR_HOME="$(mktemp -d)"
VALIDATOR2_HOME="$(mktemp -d)"
PIDS=""
cleanup() {
  for pid in $PIDS; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  rm -rf "$NODE_HOME" "$VALIDATOR_HOME" "$VALIDATOR2_HOME"
}
trap cleanup EXIT
export HOME="$NODE_HOME"

VALIDATOR_SEED_ID="12D3KooWDgCeJ15KaPzEEhV8XJd1xoN7hYBBVLdx8tBPiDgA5Lfo"
strip_ansi() { sed -E 's/\x1b\[[0-9;]*m//g' "$1"; }
count() { strip_ansi "$2" | grep -cF -- "$1"; }
# Line of the first log line holding $1 in $2, or 0.
line_of() { strip_ansi "$2" | grep -nF -- "$1" | head -1 | cut -d: -f1 | grep . || echo 0; }
# Line of the last log line holding $1 in $2, or 0.
last_line_of() { strip_ansi "$2" | grep -nF -- "$1" | tail -1 | cut -d: -f1 | grep . || echo 0; }
# Waits up to $3 seconds for a log line holding $1 in $2.
wait_for() {
  for _ in $(seq 1 $(($3 * 4))); do
    [ "$(count "$1" "$2")" -gt 0 ] && return 0
    sleep 0.25
  done
  return 1
}
rpc() {
  printf '{"jsonrpc":"2.0","method":"%s","params":%s,"id":1}' "$1" "$2" > "$LOGDIR/rpc-body.json"
  curl -s -m 2 -X POST -H 'Content-Type: application/json' \
    --data-binary @"$LOGDIR/rpc-body.json" http://127.0.0.1:8545
}
peers() { printf '{"peer_ids":["%s"]}' "$1"; }
peer_id_of() { strip_ansi "$1" | grep -o "local_peer_id=[A-Za-z0-9]*" | head -1 | cut -d= -f2; }

STATUS=0
check() { # name, condition result (0 = pass)
  if [ "$2" = 0 ]; then echo "ok   $1"; else echo "FAIL $1"; STATUS=1; fi
}
holds() { if "$@"; then echo 0; else echo 1; fi; }
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# The validators' identities, known in advance, each in its temporary home.
GENERATED=$(HOME="$VALIDATOR_HOME" "$BIN" --new-peer-id --seed-phrase validator_test)
VALIDATOR_ID=$(echo "$GENERATED" | sed -n 's/^Peer ID : //p')
check "validator identity from the seed validator_test ($VALIDATOR_ID)" \
  "$(holds [ "$VALIDATOR_ID" = "$VALIDATOR_SEED_ID" ])"
GENERATED=$(HOME="$VALIDATOR2_HOME" "$BIN" --new-peer-id --seed-phrase validator_test_2)
VALIDATOR2_ID=$(echo "$GENERATED" | sed -n 's/^Peer ID : //p')
check "second validator identity from the seed validator_test_2 ($VALIDATOR2_ID)" \
  "$(holds [ -n "$VALIDATOR2_ID" ] && [ "$VALIDATOR2_ID" != "$VALIDATOR_ID" ])"

R="$LOGDIR/relayer.log"; V="$LOGDIR/validator.log"; V2="$LOGDIR/validator2.log"
O="$LOGDIR/outsider.log"; O2="$LOGDIR/outsider2.log"

RUST_LOG=info "$BIN" --relayer --local-test --listen-address 127.0.0.1 --port 62749 > "$R" 2>&1 &
PIDS="$PIDS $!"
wait_for "local_peer_id=" "$R" 10
RELAYER_ID=$(peer_id_of "$R")
if [ -z "$RELAYER_ID" ]; then
  echo "FAIL: the relayer did not report its peer ID"; strip_ansi "$R" | tail -5; exit 1
fi
for _ in $(seq 1 40); do
  rpc authorized_nodes '{}' | grep -q '"result"' && break
  sleep 0.25
done

# The relayer is alone and idle: an RPC command must still be served at once.
# The relayer's own ID is a peer it is not connected to.
IDLE_OUT=$(rpc distribute_keys "$(peers "$RELAYER_ID")")
echo "rpc distribute_keys on the idle relayer: ${IDLE_OUT:-<no answer within 2 s>}"
check "the idle relayer answers distribute_keys within 2 seconds" \
  "$(holds contains "$IDLE_OUT" "Peer $RELAYER_ID not connected")"

# The administrator authorizes both validators before they start.
AUTHORIZE_OUT=$(rpc authorize_node "$(printf '{"peer_ids":["%s","%s"]}' "$VALIDATOR_ID" "$VALIDATOR2_ID")")
echo "rpc authorize_node: ${AUTHORIZE_OUT:-<no answer>}"
check "authorize_node lists both validators" \
  "$(holds contains "$AUTHORIZE_OUT" "\"$VALIDATOR_ID\"" && contains "$AUTHORIZE_OUT" "\"$VALIDATOR2_ID\"")"

HOME="$VALIDATOR_HOME" RUST_LOG=info "$BIN" --validator --listen-address 127.0.0.1 --port 62750 \
  --bootstrap-address 127.0.0.1 --bootstrap-port 62749 --bootstrap-peer-id "$RELAYER_ID" --gen-msg \
  > "$V" 2>&1 &
PIDS="$PIDS $!"
HOME="$VALIDATOR2_HOME" RUST_LOG=info "$BIN" --validator --listen-address 127.0.0.1 --port 62752 \
  --bootstrap-address 127.0.0.1 --bootstrap-port 62749 --bootstrap-peer-id "$RELAYER_ID" \
  > "$V2" 2>&1 &
PIDS="$PIDS $!"
RUST_LOG=info "$BIN" --validator --local-test --listen-address 127.0.0.1 --port 62751 \
  --bootstrap-address 127.0.0.1 --bootstrap-port 62749 --bootstrap-peer-id "$RELAYER_ID" --gen-msg \
  > "$O" 2>&1 &
PIDS="$PIDS $!"

# The second outsider bootstraps from the first validator directly.
wait_for "Listening on /ip4/127.0.0.1/udp/62750" "$V" 10
RUST_LOG=info "$BIN" --validator --local-test --listen-address 127.0.0.1 --port 62753 \
  --bootstrap-address 127.0.0.1 --bootstrap-port 62750 --bootstrap-peer-id "$VALIDATOR_ID" \
  > "$O2" 2>&1 &
PIDS="$PIDS $!"

# The validators find each other, and the first publishes an event.
wait_for "Connected established to $VALIDATOR2_ID" "$V" 30
wait_for "Connected established to $VALIDATOR_ID" "$V2" 10
wait_for "Got message" "$R" 15
wait_for "Denied a connection from unauthorized peer" "$V" 10
STARTED_ID=$(peer_id_of "$V")
STARTED2_ID=$(peer_id_of "$V2")
OUTSIDER_ID=$(peer_id_of "$O")
OUTSIDER2_ID=$(peer_id_of "$O2")
echo "relayer=$RELAYER_ID validator=${STARTED_ID:-<none>} validator2=${STARTED2_ID:-<none>}"
echo "outsider=${OUTSIDER_ID:-<none>} outsider2=${OUTSIDER2_ID:-<none>}"
check "the validator runs with the identity of its home" "$(holds [ "$STARTED_ID" = "$VALIDATOR_ID" ])"
check "the second validator runs with the identity of its home" "$(holds [ "$STARTED2_ID" = "$VALIDATOR2_ID" ])"

KEYS_OUT=$(rpc distribute_keys "$(peers "$VALIDATOR_ID")")
echo "rpc distribute_keys to the validator: ${KEYS_OUT:-<no answer>}"
OUTSIDER_KEYS_OUT=$(rpc distribute_keys "$(peers "${OUTSIDER_ID:-none}")")
echo "rpc distribute_keys to the outsider: ${OUTSIDER_KEYS_OUT:-<no answer>}"
wait_for "Relayer received response" "$R" 5

# Revocation of the second validator: the first closes its connection to it.
REVOKE2_OUT=$(rpc revoke_node "$(peers "$VALIDATOR2_ID")")
echo "rpc revoke_node of the second validator: ${REVOKE2_OUT:-<no answer>}"
wait_for "Disallowed peer $VALIDATOR2_ID" "$V" 10
sleep 1

# Revocation of the first validator: its connection to the relayer is closed.
REVOKE_OUT=$(rpc revoke_node "$(peers "$VALIDATOR_ID")")
echo "rpc revoke_node of the first validator: ${REVOKE_OUT:-<no answer>}"
wait_for "Connection closed with $VALIDATOR_ID" "$R" 5
AFTER_REVOKE_OUT=$(rpc distribute_keys "$(peers "$VALIDATOR_ID")")
echo "rpc distribute_keys after the revocation: ${AFTER_REVOKE_OUT:-<no answer>}"
LIST_OUT=$(rpc authorized_nodes '{}')
echo "rpc authorized_nodes: ${LIST_OUT:-<no answer>}"

echo "--- authorized validator"
check "connection" "$(holds [ "$(count "Connected established to $VALIDATOR_ID" "$R")" -gt 0 ])"
check "gossipsub messages received" "$(holds [ "$(count "Got message" "$R")" -gt 0 ])"
check "distribute_keys started" "$(holds contains "$KEYS_OUT" "Key distribution started successfully")"
check "key request received" "$(holds [ "$(count "Received key distribution request" "$V")" -gt 0 ])"
check "key response received" "$(holds [ "$(count "Relayer received response" "$R")" -gt 0 ])"
echo "--- signed tree"
check "the relayer publishes each tree it signs" \
  "$(holds [ "$(count "Published the authorized peers tree" "$R")" -ge 3 ])"
check "the relayer answers the validators' requests" \
  "$(holds [ "$(count "Sent the authorized peers tree to" "$R")" -ge 2 ])"
check "the validator accepts the tree" \
  "$(holds [ "$(count "Accepted the authorized peers tree from" "$V")" -gt 0 ])"
check "the second validator accepts the tree" \
  "$(holds [ "$(count "Accepted the authorized peers tree from" "$V2")" -gt 0 ])"
echo "--- validators"
check "the validator connects to the second through discovery" \
  "$(holds [ "$(count "Connected established to $VALIDATOR2_ID" "$V")" -gt 0 ])"
check "the second validator connects to the first" \
  "$(holds [ "$(count "Connected established to $VALIDATOR_ID" "$V2")" -gt 0 ])"
echo "--- outsider"
check "denied, and the relayer logs it" \
  "$(holds [ "$(count "Denied a connection from unauthorized peer ${OUTSIDER_ID:-none}" "$R")" -gt 0 ])"
check "never connected" "$(holds [ "$(count "Connected established to ${OUTSIDER_ID:-none}" "$R")" = 0 ])"
check "no gossipsub message from it" "$(holds [ "$(count "from peer: PeerId(\"${OUTSIDER_ID:-none}\")" "$R")" = 0 ])"
check "distribute_keys finds it not connected" "$(holds contains "$OUTSIDER_KEYS_OUT" "not connected")"
check "no key request received" "$(holds [ "$(count "Received key distribution request" "$O")" = 0 ])"
echo "--- outsider dialing the validator"
check "the validator denies it, and logs it" \
  "$(holds [ "$(count "Denied a connection from unauthorized peer ${OUTSIDER2_ID:-none}" "$V")" -gt 0 ])"
check "the validator never connected to it" \
  "$(holds [ "$(count "Connected established to ${OUTSIDER2_ID:-none}" "$V")" = 0 ])"
echo "--- revocation"
DISALLOWED_AT=$(line_of "Disallowed peer $VALIDATOR2_ID" "$V")
CLOSED2_AT=$(last_line_of "Connection closed with $VALIDATOR2_ID" "$V")
closed2_after_revocation() { [ "$DISALLOWED_AT" -gt 0 ] && [ "$CLOSED2_AT" -gt "$DISALLOWED_AT" ]; }
check "the validator closes its connection to the revoked second validator" \
  "$(holds closed2_after_revocation)"
REVOKED_AT=$(line_of "Revoked peer $VALIDATOR_ID" "$R")
CLOSED_AT=$(last_line_of "Connection closed with $VALIDATOR_ID" "$R")
closed_after_revocation() { [ "$REVOKED_AT" -gt 0 ] && [ "$CLOSED_AT" -gt "$REVOKED_AT" ]; }
check "revoke_node lists no peer" "$(holds contains "$REVOKE_OUT" '"peer_ids":[]')"
check "the validator's connection closed after the revocation" \
  "$(holds closed_after_revocation)"
check "distribute_keys finds it not connected" "$(holds contains "$AFTER_REVOKE_OUT" "Peer $VALIDATOR_ID not connected")"
check "authorized_nodes lists no peer" "$(holds contains "$LIST_OUT" '"peer_ids":[]')"
echo "--- files"
check "the validator's keypair is in its temporary home" "$(holds [ -f "$VALIDATOR_HOME/.chaincraft/keypair.key" ])"
check "the second validator's keypair is in its temporary home" "$(holds [ -f "$VALIDATOR2_HOME/.chaincraft/keypair.key" ])"
check "the --local-test relayer stored no list" "$(holds [ ! -e "$NODE_HOME/.chaincraft/authorized_peers.json" ])"
no_panic() { ! grep -q "panicked" "$R" "$V" "$V2" "$O" "$O2"; }
check "no node panicked" "$(holds no_panic)"

[ "$STATUS" = 0 ] && echo "PASS" || echo "logs: $LOGDIR"
exit "$STATUS"
