use std::error::Error;

use tracing_subscriber::EnvFilter;
use clap::Parser;
use utils::peer_id::{generate_new_keypair_and_peer_id, generate_peer_id, DefaultUserDirectoryProvider};

mod utils;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    seed_phrase: Option<String>,

    #[arg(short, long)]
    new_peer_id: bool,

    #[arg(short, long)]
    read_peer_id: bool,
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
    else {
        eprintln!("Error : You must specify --new-peer-id or --read-peer-id");
    }

    Ok(())
}
