# A RUST decentralized bridge node for relaying and validating transactions

This project is a Rust-based decentralized bridge node designed to relay and validate transactions across networks. Its purpose is to facilitate trustless interactions between disparate blockchain networks or environments, acting as a secure and decentralized intermediary.

## Current Features

- **Peer ID Generation**: Currently, the project implements functionality to generate a unique Peer ID, which will later enable node identification within the decentralized network. This feature uses cryptographic hashing to create a reproducible, secure Peer ID based on a seed phrase or, if no seed is provided, a randomly generated one.

## Installation

To use or contribute to this project, ensure you have Rust and Cargo installed. You can download them [here](https://www.rust-lang.org/tools/install).

### Build the Project

1. Clone the repository:

    ```sh
    git clone https://github.com/chaincraft-labs/bridge-nodes-rs.git && cd bridge-nodes-rs
    ```

2. Build the project

    ```sh
    cargo build
    -- OR --
    cargo build --release
    ```

3. Run tests

    ```sh
    cargo test
    ```

4. Run coverage

    ```sh
    cargo tarpaulin
    ```

## Usage

### Peer ID

1. Generate a New Peer ID

    ```sh
    cargo run -- --new-peer-id
    -- OR --
    cargo run -- --new-peer-id --seed-phrase <a seed phrase>
    ```

2. Read an existing Peer ID

    ```sh
    cargo run -- --read-peer-id
    ```

3. Show Help

    ```sh
    cargo run -- --help

    Usage: bridge-relayer-v1 [OPTIONS]

    Options:
    -s, --seed-phrase <SEED_PHRASE>
    -n, --new-peer-id
    -r, --read-peer-id
    -h, --help                       Print help
    -V, --version                    Print version
    ```

## Contributing

Contributions are welcome! For major changes, please open an issue first to discuss what you would like to add or modify.

## License

This project is licensed under the MIT License.
