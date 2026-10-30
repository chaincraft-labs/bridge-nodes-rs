# Working here

The rules of this project, for every agent and every human who works in it,
whichever tool is driving. `CLAUDE.md` imports this file.

## What this is

A permissioned peer-to-peer node network built on libp2p. Its end goal is a
decentralized bridge whose transfers are validated by threshold signatures
(MPC TSS) computed across the nodes. The TSS part does not exist yet.

- **Relayer**: the bootstrap node and the network's administrator. It is the
  only node on day one, runs the JSON-RPC server (`127.0.0.1:8545` by
  default, `--rpc-address <IP>` to change the IP) and decides who may join.
- **Validators**: nodes an administrator authorized. They join through the
  relayer, then reach each other through Kademlia and gossipsub.
- **Membership is not open.** Peer IDs are authenticated by the libp2p
  handshake. The relayer keeps a Merkle tree of the authorized peer IDs,
  signed with its key; an administrator edits it over the RPC
  (`authorize_node`, `revoke_node`, `authorized_nodes`), and the relayer
  denies a connection from any peer not in it. By design the relayer also
  distributes the signed tree so every node can check who it talks to: that
  part does not exist yet, and validators enforce nothing.

Layout: `src/main.rs` (CLI), `src/node/` (`common.rs` swarm and shared
events, `relayer.rs`, `validator.rs`, `authorized.rs` the relayer's
authorized peers and their file), `src/rpc/` (jsonrpsee server, documented in
`docs/rpc/README.md`), `src/types/` (behaviour, events, config, errors),
`src/utils/auth/` (the signed Merkle tree), `src/utils/peer_id.rs`
(identities and the keypair file), `scripts/` (`smoke.sh`, and
`run-relayer.sh`, which starts one relayer for an agent that tests the
running node).

## Build and verify

```sh
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check bans licenses sources
cargo deny check advisories
scripts/smoke.sh            # after `cargo build`; about 20 seconds
```

`cargo test` covers identities, the files under `$HOME/.chaincraft`, the
RPC's HTTP handling, and the relayer's and validator's event handling
against in-process test peers. It never runs the binary. Any change to the
swarm, the behaviours, the event loops or the RPC must also pass
`scripts/smoke.sh`, which runs a relayer, an authorized validator and an
outsider on loopback. It checks that an idle relayer answers the RPC at
once, the connection, gossipsub delivery, the key exchange triggered over
RPC, that the relayer denies the outsider, and that `revoke_node` cuts the
validator off.

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
   TCP 8545, bound to `127.0.0.1` unless `--rpc-address` says otherwise.
5. **The signed tree of authorized peers.** A leaf is
   `blake3(0x00 || peer ID bytes)`, a parent `blake3(0x01 || left || right)`,
   an unpaired node is carried up unchanged, and the root of the empty tree
   is 32 zero bytes. The relayer signs
   `"chaincraft/authorized-peers/v1" || root || timestamp` (i64, big endian),
   and every tree it signs has a greater timestamp than the one before,
   across restarts. `test_root_follows_the_documented_layout`,
   `test_known_answer_root_over_the_test_seeds` and
   `test_signed_message_is_tag_root_then_big_endian_timestamp` pin it. Once
   the tree is distributed, this is a wire format too.
6. **The authorized peers file.** Outside `--local-test`, the relayer keeps
   its current tree in `$HOME/.chaincraft/authorized_peers.json`, the JSON
   encoding of `AuthorizedPeersTree`, directory mode 0700 and file mode
   0600, written to a temporary file and then renamed. It reads it at start:
   no file means no authorized peer, and a file that does not parse, is not
   signed by the relayer's key or whose peers do not match its root stops
   the relayer and is left as it is. No test pins the JSON encoding yet: a
   change to `AuthorizedPeersTree` or `MerkleProof` changes the file, so a
   relayer that stored one before would refuse to start.

## Rules

- **Never touch a real identity or a real list.** Tests use
  `MockUserDirectoryProvider` (a temporary home). Running the binary with
  `--new-peer-id` overwrites `$HOME/.chaincraft/keypair.key`, and a relayer
  started without `--local-test` reads and writes
  `$HOME/.chaincraft/authorized_peers.json`: run either only with `HOME`
  set to a temporary directory, as `scripts/smoke.sh` and
  `scripts/run-relayer.sh` do. Node runs for testing use `--local-test`,
  which uses a random identity and reads or writes no file.
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

- The validator's run loop holds the swarm lock across
  `swarm.next().await`. It serves no RPC today, but a call that needed its
  swarm would wait until the next swarm event. The relayer's run loop owns
  its swarm and serves RPC commands through a channel.
- The JSON-RPC server is unauthenticated, and it authorizes peers: whatever
  reaches it administers the network. It listens on `127.0.0.1` unless
  `--rpc-address` says otherwise.
- Gossipsub keeps no history: a node that joins late misses every message
  published before it arrived.
