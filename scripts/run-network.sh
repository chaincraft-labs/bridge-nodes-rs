#!/usr/bin/env bash
# Starts a small network for an agent that tests or attacks the running nodes
# (nunki's `--run`): one relayer and one validator it authorized, both on
# loopback only, since nunki starts them in the agent's own container.
# Relayer: P2P on UDP 62649, JSON-RPC on TCP 8545. Validator: P2P on UDP 62650.
#
# Each node runs with a stable identity (seeds `relayer_test` and
# `validator_test`) kept in its own home under target/run-network, never the
# real `$HOME`: a restart keeps the identities and the relayer's list.
#
# Called as `run-network.sh <id>`. nunki recognises the script by that id on
# its command line, so the script stays in the foreground while the nodes run.
# It stops both nodes when it is stopped, and when either node exits.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release
BIN="$(pwd)/target/release/validator_node"
RELAYER_HOME="$(pwd)/target/run-network/relayer"
VALIDATOR_HOME="$(pwd)/target/run-network/validator"

PIDS=""
stop() {
  for pid in $PIDS; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap stop EXIT
trap 'exit 143' INT TERM

# The peer ID of the identity kept in a home, created from a seed on first use.
identity() { # home, seed
  mkdir -p "$1"
  if [ ! -f "$1/.chaincraft/keypair.key" ]; then
    HOME="$1" "$BIN" --new-peer-id --seed-phrase "$2" > /dev/null
  fi
  HOME="$1" "$BIN" --read-peer-id | sed -n 's/^Peer ID : //p'
}

rpc() { # method, params
  curl -s -m 2 -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"method\":\"$1\",\"params\":$2,\"id\":1}" \
    http://127.0.0.1:8545
}

RELAYER_ID="$(identity "$RELAYER_HOME" relayer_test)"
VALIDATOR_ID="$(identity "$VALIDATOR_HOME" validator_test)"

HOME="$RELAYER_HOME" RUST_LOG=info "$BIN" --relayer --listen-address 127.0.0.1 &
RELAYER_PID=$!
PIDS="$RELAYER_PID"

# The relayer serves the RPC once it has loaded its list.
for _ in $(seq 1 50); do
  if rpc authorized_nodes '{}' | grep -q '"result"'; then break; fi
  sleep 0.2
done

# Authorizing a peer that is already authorized changes nothing.
if ! rpc authorize_node "{\"peer_ids\":[\"$VALIDATOR_ID\"]}" | grep -q "$VALIDATOR_ID"; then
  echo "run-network: the relayer did not authorize the validator" >&2
  exit 1
fi

HOME="$VALIDATOR_HOME" RUST_LOG=info "$BIN" --validator --listen-address 127.0.0.1 \
  --bootstrap-address 127.0.0.1 --bootstrap-peer-id "$RELAYER_ID" &
VALIDATOR_PID=$!
PIDS="$PIDS $VALIDATOR_PID"

# Polled rather than `wait -n`, which macOS's bash 3.2 lacks.
while kill -0 "$RELAYER_PID" 2>/dev/null && kill -0 "$VALIDATOR_PID" 2>/dev/null; do
  sleep 1
done
echo "run-network: a node exited, stopping the other" >&2
exit 1
