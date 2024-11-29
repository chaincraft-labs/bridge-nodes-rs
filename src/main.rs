use std::error::Error;

use tracing_subscriber::EnvFilter;
use clap::Parser;
use validator_node::{node::network::Node, utils::peer_id::{generate_new_keypair_and_peer_id, generate_peer_id, DefaultUserDirectoryProvider}, NodeConfig};


#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short = 'a', long)]
    seed_phrase: Option<String>,

    #[arg(long)]
    new_peer_id: bool,

    #[arg(long)]
    read_peer_id: bool,

    #[arg(long)]
    node: bool,

    #[arg(long)]
    bootstrap_address: Option<String>,

    #[arg(long)]
    bootstrap_peer_id: Option<String>,

    #[arg(long)]
    bootstrap: bool,

    #[arg(long)]
    port: Option<usize>,

    #[arg(long)]
    bootstrap_port: Option<usize>,

    #[arg(long)]
    gen_msg: bool,

    #[arg(long)]
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
    else if args.node {
        let config = NodeConfig {
            bootstrap_address: args.bootstrap_address,
            bootstrap_peer_id: args.bootstrap_peer_id,
            is_bootstrap_node: args.bootstrap,
            is_bootstrap_started: false,
            gen_msg: args.gen_msg,
            listen_address: "0.0.0.0".to_string(),
            port: args.port.unwrap_or(62649) as u16,
            bootstrap_port: args.bootstrap_port.unwrap_or(62649) as u16,
            local_test: args.local_test,
        };
        let mut node = Node::new(config);
        node.run().await?;
    }
    else {
        eprintln!("Error : --help for more information");
    }

    Ok(())
}
