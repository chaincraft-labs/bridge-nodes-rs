# Working here

The rules of this project, for every agent and every human who works in it,
whichever tool is driving. `CLAUDE.md` imports this file.

## What this is

A permissioned peer-to-peer node network built on libp2p. Its end goal is a
decentralized bridge whose transfers are validated by threshold signatures
(MPC TSS) computed across the nodes. The TSS part does not exist yet.

- **Relayer**: the bootstrap node and the network's administrator. It is the
  only node on day one, runs the JSON-RPC server (`<listen-address>:8545`) and
  decides who may join.
- **Validators**: nodes an administrator authorized. They join through the
  relayer, then reach each other through Kademlia and gossipsub.
- **Membership is not open.** Peer IDs are authenticated by the libp2p
  handshake; by design the relayer signs the list of authorized peer IDs
  and distributes it so every node can check who it talks to. The signed
  Merkle tree of authorized peer IDs exists (`src/utils/auth/`), but nothing
  distributes or enforces it yet.

Layout: `src/main.rs` (CLI), `src/node/` (`common.rs` swarm and shared
events, `relayer.rs`, `validator.rs`), `src/rpc/` (jsonrpsee server),
`src/types/` (behaviour, events, config, errors), `src/utils/peer_id.rs`
(identities and the keypair file).

## Build and verify

```sh
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check bans licenses sources
cargo deny check advisories
scripts/smoke.sh            # after `cargo build`; about 40 seconds
```

`cargo test` covers identities, the keypair file and pure logic only. It
starts no node. Any change to the swarm, the behaviours, the event loops or
the RPC must also pass `scripts/smoke.sh`, which runs a relayer and a
validator on loopback and checks the connection, gossipsub delivery and the
key exchange triggered over RPC.

The toolchain is pinned in `rust-toolchain.toml` and must match the builder
image in `Dockerfile`: change both together.

## Invariants

These hold across every change, dependency upgrades included. If a change
would break one, do not adapt the test or the constant to the new value:
leave the invariant as it is and report the conflict.

1. **Seed phrase → peer ID.** A seed phrase is hashed with SHA3-256 into an
   ed25519 secret key. The same seed must always yield the same peer ID:
   `test_seed_phrase_yields_a_stable_peer_id` pins `relayer_test` and
   `validator_test`. A different value means every deployed node changes
   identity.
2. **The keypair file.** `$HOME/.chaincraft/keypair.key` holds the libp2p
   protobuf encoding of the keypair, in standard base64, directory mode 0700
   and file mode 0600. Files written by earlier versions must still load:
   `test_keypair_file_format_is_stable` and
   `test_existing_keypair_file_is_read_back` pin it.
3. **Wire names.** Peers built from different commits must keep talking:
   the request-response protocol `/key-exchange/1.0.0`, the gossipsub topic
   `custom_events`, the identify protocol `chaincraft-p2p/1.0.0` and the
   Kademlia protocol `/ipfs/kad/1.0.0`. Change one only together with a
   version bump of that protocol.
4. **Default ports.** P2P on UDP 62649 (relayer) and 62650 (validator); RPC on
   TCP 8545.

## Rules

- **Never touch a real identity.** Tests use `MockUserDirectoryProvider`
  (a temporary home). Running the binary with `--new-peer-id` overwrites
  `$HOME/.chaincraft/keypair.key`: run it only with `HOME` set to a
  temporary directory. Node runs for testing use `--local-test`, which uses
  a random identity and leaves the file alone.
- **English only** in code, comments, docstrings, log messages and commit
  messages.
- **Formatting is rustfmt's default**, and clippy runs with `-D warnings`. An
  `#[allow(clippy::…)]` is scoped to one item and carries a comment saying
  why.
- **Errors**: return `NodeError` (nodes) or `RpcError` (RPC) rather than
  panicking. Data received from the network is untrusted: never `unwrap()` or
  index into it unchecked in new code.
- **Logging** goes through `tracing`. `println!` is for the CLI's own output
  in `src/main.rs` (the peer ID it prints) and for tests.
- **Dependencies**: a new or upgraded crate must pass `deny.toml` as it is.
  Do not widen the license allow-list, and do not add an advisory to
  `ignore` without an `id` and a `reason`. Keep `Cargo.lock` committed.
- **Licensing is undecided.** The crate has no license on purpose
  (`publish = false`): do not add a `LICENSE` file or a `license` field.

## Known issues

Recorded so they are not mistaken for regressions. Fix one only when the task
asks for it.

- The run loops hold the swarm lock across `swarm.next().await`, so an RPC
  call that needs the swarm can wait until the next swarm event.
- The JSON-RPC server is unauthenticated and listens on the node's listen
  address.
- Gossipsub keeps no history: a node that joins late misses the authorized
  list published before it arrived.
