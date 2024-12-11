use std::error::Error;

use tracing_subscriber::EnvFilter;
use clap::Parser;
use validator_node::{node::{relayer::RelayerNode, validator::ValidatorNode}, utils::peer_id::{generate_new_keypair_and_peer_id, generate_peer_id, DefaultUserDirectoryProvider}, NodeConfig};


#[derive(Parser, Debug)]
#[command(
    name = "validator-node",
    version,
    about = "A validator node implementation for blockchain networks",
    long_about = r#"A validator node implementation for blockchain networks. This application implements a validator node that can operate either as a validator or relayer in a blockchain network. It supports peer-to-peer connectivity and custom network configurations.

EXAMPLES:
    # 1. Peer ID Management:
    # Generate a new random peer ID
    validator-node --new-peer-id
    
    # Generate a deterministic peer ID using a seed phrase
    validator-node --new-peer-id --seed-phrase "my secret phrase"
    
    # Read an existing peer ID from storage
    validator-node --read-peer-id

    # 2. Running a Relayer Node:
    # Start a basic relayer node with default settings
    validator-node --relayer
    
    # Start a relayer on a specific port and address
    validator-node --relayer --port 8000 --listen-address "192.168.1.10"
    
    # Start a relayer in local testing mode
    validator-node --relayer --local-test

    # 3. Running a Validator Node:
    # Start a validator connected to a bootstrap node
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --bootstrap-port 62650
    
    # Start a validator with custom port and message generation
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --port 9000 \
        --gen-msg

    # Start a validator in local testing mode
    validator-node --validator \
        --bootstrap-address "127.0.0.1" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --local-test"#
)]
struct Args {
    #[arg(
        short = 'a',
        long,
        help = "Optional seed phrase to generate a deterministic peer ID",
        long_help = r#"Provide a seed phrase to generate a deterministic peer ID. This ensures the same peer ID is generated each time when using the same seed phrase. If not provided, a random peer ID will be generated.

Example: 
    validator-node --new-peer-id --seed-phrase "my secure seed phrase""#,
        value_name = "SEED"
    )]
    seed_phrase: Option<String>,

    #[arg(
        long,
        help = "Generate a new peer ID",
        long_help = r#"Generate and save a new peer ID to the local storage. If used with --seed-phrase, generates a deterministic peer ID.

Examples:
    # Generate random peer ID
    validator-node --new-peer-id
    
    # Generate deterministic peer ID
    validator-node --new-peer-id --seed-phrase "my phrase""#,
        conflicts_with = "read_peer_id"
    )]
    new_peer_id: bool,

    #[arg(
        long,
        help = "Read the existing peer ID from storage",
        long_help = r#"Read and display the peer ID stored in local storage. Returns an error if no peer ID exists.

Example:
    validator-node --read-peer-id"#
    )]
    read_peer_id: bool,

    #[arg(
        long,
        help = "Run as a validator node",
        long_help = r#"Start the application as a validator node. Validators participate in consensus and validate transactions.

Examples:
    # Basic validator setup
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..."

    # Validator with custom port and message generation
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --port 9000 \
        --gen-msg"#,
        conflicts_with = "relayer"
    )]
    validator: bool,

    #[arg(
        long,
        help = "Run as a relayer node",
        long_help = r#"Start the application as a relayer node. Relayers help propagate messages across the network.

Examples:
    # Basic relayer setup
    validator-node --relayer

    # Relayer with custom configuration
    validator-node --relayer \
        --port 8000 \
        --listen-address "192.168.1.10""#,
        conflicts_with = "validator"
    )]
    relayer: bool,

    #[arg(
        long,
        help = "Bootstrap node address",
        long_help = r#"The IP address or hostname of the bootstrap node to connect to. Required when running as a validator node.

Example:
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX...""#,
        value_name = "ADDRESS",
        requires = "bootstrap_peer_id"
    )]
    bootstrap_address: Option<String>,

    #[arg(
        long,
        help = "Bootstrap node peer ID",
        long_help = r#"The peer ID of the bootstrap node to connect to. Required when running as a validator node.

Example:
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX...""#,
        value_name = "PEER_ID",
        requires = "bootstrap_address"
    )]
    bootstrap_peer_id: Option<String>,

    #[arg(
        long,
        help = "Bootstrap node port",
        long_help = r#"The port number of the bootstrap node. Defaults to 62650 for validator nodes.

Example:
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --bootstrap-port 62650"#,
        value_name = "PORT",
        requires = "bootstrap_address"
    )]
    bootstrap_port: Option<u16>,

    #[arg(
        long,
        help = "Local node port",
        long_help = r#"The port number this node will listen on. Defaults to 62650 for validator nodes and 62649 for relayer nodes.

Examples:
    # Custom port for validator
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --port 9000

    # Custom port for relayer
    validator-node --relayer --port 8000"#,
        value_name = "PORT"
    )]
    port: Option<u16>,

    #[arg(
        long,
        help = "Local listen address",
        long_help = r#"The IP address this node will listen on. Defaults to 0.0.0.0 (all interfaces).

Examples:
    # Specific interface for validator
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --listen-address "192.168.1.10"

    # Specific interface for relayer
    validator-node --relayer --listen-address "192.168.1.10""#,
        value_name = "ADDRESS"
    )]
    listen_address: Option<String>,

    #[arg(
        long,
        help = "Enable message generation",
        long_help = r#"Enable automatic message generation for testing purposes. Only applicable for validator nodes.

Example:
    validator-node --validator \
        --bootstrap-address "192.168.1.100" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --gen-msg"#
    )]
    gen_msg: bool,

    #[arg(
        long,
        help = "Enable local testing mode",
        long_help = r#"Run the node in local testing mode with modified parameters suitable for local development and testing.

Examples:
    # Local testing for validator
    validator-node --validator \
        --bootstrap-address "127.0.0.1" \
        --bootstrap-peer-id "12D3KooWXXXX..." \
        --local-test

    # Local testing for relayer
    validator-node --relayer --local-test"#
    )]
    local_test: bool,
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let args = Args::parse();
    let user_dir_provider = DefaultUserDirectoryProvider;

    if args.new_peer_id && ! args.read_peer_id {
        match generate_new_keypair_and_peer_id(args.seed_phrase.as_deref(), &user_dir_provider) {
            Ok(peer_id) => {
                println!("Peer ID : {}", peer_id);
            }
            Err(e) => {
                eprintln!("Error generating Peer ID : {}", e);
            }
        }
    }
    else if args.read_peer_id {
        match generate_peer_id(&user_dir_provider) {
            Ok(peer_id) => {
                println!("Peer ID : {}", peer_id);
            }
            Err(e) => {
                eprintln!("Error reading Peer ID : {}", e);
            }
        }
    }
    else if args.validator {
        const DEFAULT_PORT: u16 = 62650;
        let config = NodeConfig {
            listen_address: args.listen_address.unwrap_or("0.0.0.0".to_string()),
            port: args.port.unwrap_or(DEFAULT_PORT),
            is_bootstrap_node: false,
            bootstrap_peer_id: args.bootstrap_peer_id,
            bootstrap_address: args.bootstrap_address,
            bootstrap_port: args.bootstrap_port.unwrap_or(DEFAULT_PORT),
            is_bootstrap_started: false,
            gen_msg: args.gen_msg,
            local_test: args.local_test,
        };
        let mut node = ValidatorNode::new(config);
        node.run().await?;
    }
    else if args.relayer {
        const DEFAULT_PORT: u16 = 62649;
        let config = NodeConfig {
            listen_address: args.listen_address.unwrap_or("0.0.0.0".to_string()),
            port: args.port.unwrap_or(DEFAULT_PORT),
            is_bootstrap_node: true,
            bootstrap_peer_id: None,
            bootstrap_address: None,
            bootstrap_port: 0,
            is_bootstrap_started: false,
            gen_msg: false,
            local_test: args.local_test,
        };
        let mut node = RelayerNode::new(config);
        node.run().await?;
    }
    else {
        eprintln!("Error : --help for more information");
    }

    Ok(())
}
