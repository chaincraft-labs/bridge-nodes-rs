#!/bin/sh
# Starts one relayer for an agent that tests or attacks the running node
# (nunki's `--run`): P2P on UDP 62649, JSON-RPC on TCP 8545, both on
# loopback only, since nunki starts it in the agent's own container.
#
# The relayer runs with a stable identity (seed `relayer_test`) kept in its
# own home, target/run-home, never the real `$HOME`: a restart keeps the
# identity and whatever the relayer stored there.
#
# Called as `run-relayer.sh <id>`. nunki recognises the process by that id on
# its command line, so the relayer is not `exec`ed, and the script stays in
# the foreground for as long as the relayer runs.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
NODE_HOME="$(pwd)/target/run-home"
mkdir -p "$NODE_HOME"
if [ ! -f "$NODE_HOME/.chaincraft/keypair.key" ]; then
  HOME="$NODE_HOME" target/release/validator_node --new-peer-id --seed-phrase relayer_test
fi
HOME="$NODE_HOME" RUST_LOG=info target/release/validator_node --relayer --listen-address 127.0.0.1
