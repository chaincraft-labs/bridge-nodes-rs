# Decentralized Network Infrastructure

This project is a Rust-based decentralized bridge node designed to validate transactions across networks. Its purpose is to facilitate trustless interactions between disparate blockchain networks or environments, acting as a secure and decentralized intermediary.

## Current Features

- **Peer ID Generation**: Implements functionality to generate a unique Peer ID, enabling node identification within the decentralized network. This feature uses cryptographic hashing to create a reproducible, secure Peer ID based on a seed phrase or, if no seed is provided, a randomly generated one.

- **Decentralized Network**: Implements a robust P2P network infrastructure featuring:
  - QUIC transport protocol for secure, low-latency communication
  - Kademlia DHT for efficient peer discovery and routing
  - GossipSub for reliable pub/sub message propagation
  - Dual node types:
    - Bootstrap nodes: Network entry points providing initial peer discovery
    - Regular nodes: Network participants handling routing and message propagation

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
    docker build -t bridge-validator-node:latest .
    docker compose -f DockerCompose.yml up
    ```

2. Connect to containers

    Bootstrap node:

    ```sh
    docker exec --privileged -ti node_kdm_bootstrap /bin/bash
    ```

    Regular nodes (1-5):

    ```sh
    docker exec --privileged -ti node_kdm_client1 /bin/bash
    # Repeat for client2 through client5
    ```

## Usage

> Note: For release builds, replace `cargo run -- <opt>` with `./target/release/bridge-relayer-v1 <opt>`

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

1. Start a bootstrap node

    ```sh
    ./target/release/bridge-relayer-v1 --node-kad --bootstrap --authorized-peer-id <peer id>
    ```

    > Note: `--authorized-peer-id` is used for testing to specify event generation permissions

2. Start a regular node

    ```sh
    ./target/release/bridge-relayer-v1 --node-kad --bootstrap-address <ip address or hostname> --bootstrap-peer-id <peer id> --authorized-peer-id <peer id>
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

## License

This project is licensed under the MIT License. See the LICENSE file for details.
