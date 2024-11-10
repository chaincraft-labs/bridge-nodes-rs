use std::error::Error;
use libp2p::PeerId;
use tracing_subscriber::EnvFilter;
use clap::Parser;
use sha3::{Sha3_256, Digest};
use base64::{engine::general_purpose, Engine};
use std::fs::File;
use std::io::{Write, Read};
use general_purpose::STANDARD;

const DEFAULT_PATH: &str = "keypair.key";


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


fn generate_peer_id() -> Result<PeerId, Box<dyn Error>> {
    let mut file = File::open(DEFAULT_PATH)?;
    let mut encoded_secret_base64 = String::new();
    file.read_to_string(&mut encoded_secret_base64)?;

    let encoded_secret = STANDARD.decode(&encoded_secret_base64)?;
    let keypair = libp2p::identity::Keypair::from_protobuf_encoding(&encoded_secret)?;

    Ok(PeerId::from(keypair.public()))
}

fn generate_new_peer_id(seed_phrase: &str) -> Result<PeerId, Box<dyn Error>> {
    let secret_key_seed = seed_phrase_to_bytes(seed_phrase);
    let keypair =  generate_keypair(secret_key_seed);
    save_keypair(&keypair)?;
    generate_peer_id()
}

fn generate_keypair(secret_key_seed: Option<[u8; 32]>) -> libp2p::identity::Keypair {
    match secret_key_seed {
        Some(seed) => libp2p::identity::Keypair::ed25519_from_bytes(seed).unwrap(),
        None => libp2p::identity::Keypair::generate_ed25519(),
    }
}

fn save_keypair(keypair: &libp2p::identity::Keypair)-> Result<(), Box<dyn Error>> {
    // convert the key encoded protobuf
    let encoded_secret = keypair.to_protobuf_encoding()?;

    // Encode the key in base64 in order to save to the file
    let encoded_secret_base64 = general_purpose::STANDARD.encode(&encoded_secret);

    // save the key in the file
    let mut file = File::create(DEFAULT_PATH)?;
    file.write_all(encoded_secret_base64.as_bytes())?;

    Ok(())
}

fn seed_phrase_to_bytes(seed_phrase: &str) -> Option<[u8; 32]> {
    let mut hasher = Sha3_256::new();
    hasher.update(seed_phrase);
    let result = hasher.finalize();

    result.as_slice().try_into().ok()
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let args = Args::parse();

    if args.new_peer_id && args.seed_phrase.is_some() {
        match generate_new_peer_id(args.seed_phrase.unwrap().as_str()) {
            Ok(peer_id) => {
                println!("Peer ID : {}", peer_id);
            }
            Err(e) => {
                eprintln!("Error generating Peer ID : {}", e);
            }
        }
    }
    else if args.read_peer_id {
        match generate_peer_id() {
            Ok(peer_id) => {
                println!("Peer ID : {}", peer_id);
            }
            Err(e) => {
                eprintln!("Error reading Peer ID : {}", e);
            }
        }
    }
    else {
        eprintln!("Erreur : Vous devez spécifier au moins --new-peer-id ou --read-peer-id");
    }

    Ok(())
}