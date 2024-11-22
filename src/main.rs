use std::error::Error;

use tracing_subscriber::EnvFilter;
use clap::Parser;
use utils::peer_id::{generate_new_keypair_and_peer_id, generate_peer_id, DefaultUserDirectoryProvider};

mod utils;
mod nodes;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short = 's', long)]
    seed_phrase: Option<String>,

    #[arg(short = 'p', long)]
    new_peer_id: bool,

    #[arg(short = 'r', long)]
    read_peer_id: bool,

    #[arg(short = 'x', long)]
    node_rdv: bool,

    #[arg(short = 'y', long)]
    node: bool,

    #[arg(short = 'a', long)]
    rdv_point_address: Option<String>,

    #[arg(short = 'b', long)]
    rdv_point_peer_id: Option<String>,

    #[arg(short = 'c', long)]
    external_address: Option<String>,

    // kademlia
    #[arg(short = 'k', long)]
    node_kad: bool,

    #[arg(short = 'l', long)]
    bootstrap_address: Option<String>,

    #[arg(short = 'm', long)]
    bootstrap_peer_id: Option<String>,

    #[arg(short = 'n', long)]
    bootstrap: bool,

    #[arg(short = '0', long)]
    authorized_peer_id: Option<String>,
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
    else if args.node_rdv && args.rdv_point_address.is_some() {
        nodes::node_rdv::run(
            args.rdv_point_address.as_deref().unwrap()
        ).await.unwrap();
    }
    else if args.node_kad &&
        args.bootstrap_peer_id.is_some() &&
        args.bootstrap_address.is_some() {
        nodes::node_kad::run(
            args.bootstrap_address.as_deref(),
            args.bootstrap_peer_id.as_deref(),
            args.bootstrap,
            args.authorized_peer_id.as_deref(),
        ).await.unwrap();
    }
    else if args.node_kad && args.bootstrap {
        nodes::node_kad::run(
            None,
            None,
            args.bootstrap,
            args.authorized_peer_id.as_deref(),
        ).await.unwrap();
    }
    else if args.node &&
        args.rdv_point_address.is_some() &&
        args.rdv_point_peer_id.is_some() &&
        args.external_address.is_some() {
        nodes::node::run(
            args.rdv_point_address.as_deref().unwrap(),
            args.rdv_point_peer_id.as_deref().unwrap(),
            args.external_address.as_deref().unwrap(),
        ).await.unwrap();
    }
    else {
        eprintln!("Error : --help for more information");
    }

    Ok(())
}
