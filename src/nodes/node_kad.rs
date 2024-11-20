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
    bootstrap_address: Option<&str>,
    bootstrap_peer_id: Option<&str>,
    bootstrap: bool,
) -> Result<(), Box<dyn std::error::Error>> {

    let user_dir_provider = DefaultUserDirectoryProvider;
    let keypair = read_keypair_from_file(&user_dir_provider)?;

    let mut swarm = SwarmBuilder::with_existing_identity(keypair.clone())
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

    swarm.listen_on("/ip4/0.0.0.0/udp/62649/quic-v1".parse()
        .map_err(|e| format!("Parse error: {}", e))?)
        .map_err(|e| format!("Listen error: {}", e))?;

    swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Server));

    // if bootstrap {
    //     // let local_address: Multiaddr = format!("/ip4/127.0.0.1/udp/62649/quic-v1").parse::<Multiaddr>().unwrap();
    //     // let local_address: Multiaddr = format!("/ip4/192.168.1.64/udp/62649/quic-v1").parse::<Multiaddr>().unwrap();
    //     let peer_id = &keypair.clone().public().to_peer_id();
    //     tracing::info!("Node set as bootstrap with peer id {}", peer_id);

    //     swarm.behaviour_mut()
    //         .kademlia.add_address(peer_id, "/dnsaddr/bootstrap.libp2p.io".parse()?);
    //     // swarm.behaviour_mut()
    //     //     .kademlia.bootstrap()
    //     //     .map_err(|e| format!("Bootstrap error: {}", e))?;
    // } else {
    //     if bootstrap_address.is_none() || bootstrap_peer_id.is_none() {
    //         return Err(Box::new(std::io::Error::new(
    //             std::io::ErrorKind::Other,
    //             "Missing bootstrap address or peer ID",
    //         )));
    //     }

    //     let bootstrap_peer_id: PeerId = PeerId::from_str(bootstrap_peer_id.unwrap()).unwrap();
    //     let bootstrap_address: Multiaddr = format!("/ip4/{}/udp/62649/quic-v1", bootstrap_address.unwrap()).parse::<Multiaddr>().unwrap();
    //     tracing::info!("Node set with bootstrap peer id {} and address {}", bootstrap_peer_id, bootstrap_address);
    //     swarm.behaviour_mut()
    //         .kademlia.add_address(&bootstrap_peer_id, bootstrap_address.clone());

    //     swarm.behaviour_mut()
    //         .kademlia.bootstrap()
    //         .map_err(|e| format!("Bootstrap error: {}", e))?;
    // }

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



