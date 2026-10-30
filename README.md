# Decentralized Network Infrastructure

This project is a Rust-based decentralized bridge node designed to validate transactions across networks. Its purpose is to facilitate trustless interactions between disparate blockchain networks or environments, acting as a secure and decentralized intermediary.

## Current Features

- **Peer ID Generation**: Implements functionality to generate a unique Peer ID, enabling node identification within the decentralized network. This feature uses cryptographic hashing to create a reproducible, secure Peer ID based on a seed phrase or, if no seed is provided, a randomly generated one.

- **Decentralized Network**: Implements a robust P2P network infrastructure featuring:
  - QUIC transport protocol for secure, low-latency communication
  - Kademlia DHT for efficient peer discovery and routing
  - GossipSub for reliable pub/sub message propagation
  - Two node types:
    - Relayer: the bootstrap node, the network's entry point and administrator, which also serves the JSON-RPC API
    - Validators: nodes that join through the relayer, then handle routing and message propagation

## Installation

### Prerequisites

- Rust and Cargo ([install here](https://www.rust-lang.org/tools/install))
- (Optional) Docker and Docker Compose for containerized deployment

### Build Steps

1. Clone the repository

    ```sh
    git clone https://github.com/chaincraft-labs/bridge-nodes-rs.git && cd bridge-nodes-rs
    ```

2. Build the project

    ```sh
    cargo build
    # OR for optimized release
    cargo build --release
    ```

3. Run tests

    ```sh
    cargo test
    ```

4. Generate code coverage

    ```sh
    cargo tarpaulin
    ```

### Docker Deployment

1. Build the image and create containers

    ```sh
    docker build -t libp2p-node:latest .
    docker compose -f DockerCompose.yml up
    ```

2. Connect to containers

    Relayer (bootstrap node):

    ```sh
    docker exec --privileged -ti node_kdm_bootstrap /bin/bash
    ```

    Validators (1-5):

    ```sh
    docker exec --privileged -ti node_kdm_client1 /bin/bash
    # Repeat for client2 through client5
    ```

## Usage

> Note: Replace **cargo run -- --args** with **./target/release/validator_node --args** when you ran **cargo build --release**. `--help` lists every option.

### Peer ID Operations

1. Generate a new Peer ID

    ```sh
    cargo run -- --new-peer-id
    # Or with a specific seed phrase
    cargo run -- --new-peer-id --seed-phrase <seed phrase>
    ```

1. Read an existing Peer ID

    ```sh
    cargo run -- --read-peer-id
    ```

### Node Operations

1. Start a relayer

    ```sh
    RUST_LOG=info cargo run -- --relayer

    # [optional] --port <port> (default is 62649, UDP)
    # [optional] --listen-address <ip> (default is 0.0.0.0)
    # [optional] --local-test : use a random peer ID and leave the keypair file alone
    ```

    The relayer logs its peer ID at start (`local_peer_id=...`) and serves the JSON-RPC API on port 8545 (see [docs/rpc](docs/rpc/README.md)).

2. Start a validator

    ```sh
    RUST_LOG=info cargo run -- --validator --bootstrap-address <address> --bootstrap-peer-id <peer_id>

    # Example: a relayer running on the same machine
    RUST_LOG=info cargo run -- --validator --bootstrap-address 127.0.0.1 --bootstrap-peer-id <relayer peer ID>

    # [optional] --port <port> (default is 62650, UDP)
    # [optional] --bootstrap-port <port> (default is 62649, the relayer's default)
    # [optional] --listen-address <ip> (default is 0.0.0.0)
    # [optional] --local-test : use a random peer ID and leave the keypair file alone
    # [optional] --gen-msg : publish test messages through gossipsub
    ```

### Network Configuration

The decentralized network supports:

<!-- - Up to 20 nodes for optimal performance -->
- Secure communication via QUIC protocol
- Efficient peer discovery through Kademlia DHT
- Reliable message broadcasting with GossipSub

## Contributing

Contributions are welcome! For major changes:

1. Fork the repository
2. Create a feature branch
3. Open an issue to discuss proposed changes
4. Submit a Pull Request

Please ensure to update tests as appropriate.
