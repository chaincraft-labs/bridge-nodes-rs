use futures::StreamExt;
use libp2p::{
    noise, ping, rendezvous, swarm::{NetworkBehaviour, SwarmEvent}, tcp, yamux, Multiaddr, PeerId
};
use std::time::Duration;

use crate::utils::peer_id::{read_keypair_from_file, DefaultUserDirectoryProvider};


#[derive(NetworkBehaviour)]
struct MyBehaviour {
    rendezvous: rendezvous::client::Behaviour,
    ping: ping::Behaviour,
}

pub async fn run(
    rendezvous_point_address: &str,
    rendezvous_point_peer_id: &str,
    external_address: &str,
) -> Result<(), Box<dyn std::error::Error>> {

    let rendezvous_point_address: Multiaddr = format!("/ip4/{rendezvous_point_address}/tcp/62649").parse::<Multiaddr>().unwrap();
    let rendezvous_point_peer_id: PeerId = rendezvous_point_peer_id
        .parse()
        .unwrap();

    let user_dir_provider = DefaultUserDirectoryProvider;
    let keypair = read_keypair_from_file(&user_dir_provider)?;

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| MyBehaviour {
            rendezvous: rendezvous::client::Behaviour::new(key.clone()),
            ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(1))),
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(5)))
        .build();

    // In production the external address should be the publicly facing IP address of the rendezvous point.
    // This address is recorded in the registration entry by the rendezvous point.
    let external_address = format!("/ip4/{external_address}/tcp/0").parse::<Multiaddr>().unwrap();
    swarm.add_external_address(external_address);
    swarm.dial(rendezvous_point_address.clone()).unwrap();

    while let Some(event) = swarm.next().await {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!("Listening on {}", address);
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                cause: Some(error),
                ..
            } if peer_id == rendezvous_point_peer_id => {
                tracing::error!("Lost connection to rendezvous point {}", error);
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == rendezvous_point_peer_id => {
                if let Err(error) = swarm.behaviour_mut().rendezvous.register(
                    rendezvous::Namespace::from_static("rendezvous"),
                    rendezvous_point_peer_id,
                    None,
                ) {
                    tracing::error!("Failed to register: {error}");
                    return Err(Box::new(error) as Box<dyn std::error::Error>);
                }
                tracing::info!("Connection established with rendezvous point {}", peer_id);
            }
            // once `/identify` did its job, we know our external address and can register
            SwarmEvent::Behaviour(MyBehaviourEvent::Rendezvous(
                rendezvous::client::Event::Registered {
                    namespace,
                    ttl,
                    rendezvous_node,
                },
            )) => {
                tracing::info!(
                    "Registered for namespace '{}' at rendezvous point {} for the next {} seconds",
                    namespace,
                    rendezvous_node,
                    ttl
                );
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::Rendezvous(
                rendezvous::client::Event::RegisterFailed {
                    rendezvous_node,
                    namespace,
                    error,
                },
            )) => {
                tracing::error!(
                    "Failed to register: rendezvous_node={}, namespace={}, error_code={:?}",
                    rendezvous_node,
                    namespace,
                    error
                );
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "Failed to register: rendezvous_node={}, namespace={}, error_code={:?}",
                        rendezvous_node, namespace, error
                    ),
                )));
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::Ping(ping::Event {
                peer,
                result: Ok(rtt),
                ..
            })) if peer != rendezvous_point_peer_id => {
                tracing::info!("Ping to {} is {}ms", peer, rtt.as_millis())
            }
            other => {
                tracing::debug!("Unhandled {:?}", other);
            }
        }
    }
    Ok(())
}
