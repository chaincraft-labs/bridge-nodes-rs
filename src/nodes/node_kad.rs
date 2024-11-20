use futures::StreamExt;
use libp2p::{
    gossipsub, identify, kad, ping, swarm::{NetworkBehaviour, SwarmEvent}, Multiaddr, PeerId, SwarmBuilder
};
use libp2p::kad::store::MemoryStore;
use libp2p::kad::Mode;
use std::{str::FromStr, time::Duration};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use tokio::io;

use crate::utils::peer_id::{read_keypair_from_file, DefaultUserDirectoryProvider};



#[derive(NetworkBehaviour)]
struct MyBehaviour {
    gossipsub: gossipsub::Behaviour,
    kademlia: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

pub async fn run(
    bootstrap_address: &str,
    bootstrap_peer_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {

    let bootstrap_peer_id: PeerId = PeerId::from_str(bootstrap_peer_id).unwrap();
    let bootstrap_address: Multiaddr = format!("/ip4/{bootstrap_address}/udp/62649/quic-v1").parse::<Multiaddr>().unwrap();

    let user_dir_provider = DefaultUserDirectoryProvider;
    let keypair = read_keypair_from_file(&user_dir_provider)?;

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_quic()
        .with_dns()?
        .with_behaviour(|key |{
            let message_id_fn = |message: &gossipsub::Message| {
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            };

            // Set a custom gossipsub configuration
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
                .validation_mode(gossipsub::ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message signing)
                .message_id_fn(message_id_fn) // content-address messages. No two messages of the same content will be propagated.
                .build()
                .map_err(|msg| io::Error::new(io::ErrorKind::Other, msg))?; // Temporary hack because `build` does not return a proper `std::error::Error`.

            Ok(MyBehaviour {
                // build a gossipsub network behaviour
                gossipsub: gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                )?,
                kademlia: kad::Behaviour::new(
                    key.public().to_peer_id(),
                    MemoryStore::new(key.public().to_peer_id()),
                ),
                identify: identify::Behaviour::new(identify::Config::new(
                    "chaincraft-node-rdv/1.0.0".to_string(),
                    key.public(),
                )),
                ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(1))),
            })
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(5)))
        .build();


    swarm.behaviour_mut()
        .kademlia.add_address(&bootstrap_peer_id, bootstrap_address.clone());
    swarm.behaviour_mut()
        .kademlia.set_mode(Some(Mode::Server));

    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()
        .map_err(|e| format!("Parse error: {}", e))?)
        .map_err(|e| format!("Listen error: {}", e))?;


    loop {
        let event = swarm.select_next_some().await;

        match event {
            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                tracing::info!("Connected to {} via {:?}", peer_id, endpoint);
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!("Listening on {}", address);
            }
            _ => tracing::debug!("Other event: {:?}", event),
        }
    }
}



