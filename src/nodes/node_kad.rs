use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::{
    io::{self},
    select,
};
use libp2p::{
    gossipsub::{self}, identify, kad, noise, ping, swarm::{NetworkBehaviour, SwarmEvent}, tcp, yamux, Multiaddr, PeerId, SwarmBuilder
};
use libp2p::kad::store::MemoryStore;
use libp2p::kad::Mode;
use serde::{Deserialize, Serialize};
use std::{str::FromStr, time::Duration};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;


use crate::utils::peer_id::{read_keypair_from_file, DefaultUserDirectoryProvider};


// for testin
#[derive(Debug, Serialize, Deserialize)]
struct CustomEvent {
    timestamp: u64,
    event_type: String,
    data: String,
}


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
    authorized_peer_id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {

    let user_dir_provider = DefaultUserDirectoryProvider;
    let keypair = read_keypair_from_file(&user_dir_provider)?;

    let mut swarm = SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        // .with_tcp(
        //     tcp::Config::default(),
        //     noise::Config::new,
        //     yamux::Config::default,
        // )?
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

    // Create a Gossipsub topic
    let gossipsub_topic = gossipsub::IdentTopic::new("custom_events");
    // Subscribe to the topic
    tracing::info!("Subscribing to {gossipsub_topic:?}");
    swarm
        .behaviour_mut()
        .gossipsub
        .subscribe(&gossipsub_topic)
        .unwrap();

    swarm.listen_on("/ip4/0.0.0.0/udp/62649/quic-v1".parse()?)?;
    // swarm.listen_on("/ip4/0.0.0.0/udp/62649/quic-v1".parse()
    //     .map_err(|e| format!("Parse error: {}", e))?)
    //     .map_err(|e| format!("Listen error: {}", e))?;
    // swarm.listen_on("/ip4/0.0.0.0/tcp/62649".parse()
    //     .map_err(|e| format!("Parse error: {}", e))?)
    //     .map_err(|e| format!("Listen error: {}", e))?;

    swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Server));

    if !bootstrap {
        if bootstrap_address.is_none() || bootstrap_peer_id.is_none() {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Missing bootstrap address or peer ID",
            )));
        }

        let bootstrap_peer_id: PeerId = PeerId::from_str(bootstrap_peer_id.unwrap()).unwrap();
        let bootstrap_addr = bootstrap_address.unwrap();
        let bootstrap_address: Multiaddr = if bootstrap_addr.parse::<std::net::IpAddr>().is_ok() {
            format!("/ip4/{}/udp/62649/quic-v1", bootstrap_addr)
        } else {
            format!("/dns4/{}/udp/62649/quic-v1", bootstrap_addr)
        }.parse::<Multiaddr>().unwrap();

        // Ou version avec DNS et sous-domaines
        // let bootstrap_address: Multiaddr = format!("/dnsaddr/{}/tcp/62649", bootstrap_address.unwrap())
        //     .parse::<Multiaddr>()
        //     .unwrap();

        tracing::info!(
            "Node starting with peer id {} and connecting to bootstrap node {} at {}",
            keypair.public().to_peer_id(),
            bootstrap_peer_id,
            bootstrap_address,
        );

        // Add bootstrap node to routing table
        swarm.behaviour_mut()
            .kademlia.add_address(&bootstrap_peer_id, bootstrap_address.clone());

        // Wait for swarm to be ready
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Run bootstrap
        match swarm.behaviour_mut().kademlia.bootstrap() {
            Ok(_) => tracing::info!("Bootstrap process started"),
            Err(e) => tracing::error!("Failed to start bootstrap: {}", e),
        }
    }

    // ONLY for testing :: simulate event from blockchain
    // Specific peer ID that's allowed to generate events
    // Wrap swarm in Arc<Mutex>
    let swarm = Arc::new(Mutex::new(swarm));
    // Clone Arc for event_generation
    let swarm_event = Arc::clone(&swarm);
    let gossipsub_topic_clone = gossipsub_topic.clone();
    // Clone `authorized_peer_id` to extend its lifetime.
    let authorized_peer_id = authorized_peer_id.map(|id| id.to_string());

    // Set up periodic event generation if we're the authorized peer
    let mut event_generation = tokio::spawn(async move {
        let current_peer_id = keypair.public().to_peer_id().to_string();

        if let Some(auth_peer_id) = authorized_peer_id {
            if current_peer_id == auth_peer_id {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;

                    let event = CustomEvent {
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                        event_type: "ExampleEvent".to_string(),
                        data: "Some event data".to_string(),
                    };

                    let message = serde_json::to_string(&event).unwrap();

                    // Lock swarm only when needed
                    let mut swarm = swarm_event.lock().await;

                    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(
                        gossipsub_topic_clone.clone(),
                        message.as_bytes(),
                    ) {
                        tracing::error!("Failed to publish event: {:?}", e);
                    } else {
                        tracing::info!("Event published successfully");
                    }
                }
            }
        }

        Ok::<(), Box<dyn std::error::Error + Send>>(())
    });

    let mut event_generation_completed = false;

    loop {
        select! {
            result = &mut event_generation, if !event_generation_completed => {
                match result {
                    Ok(Ok(())) => {
                        tracing::info!("Event generation task completed successfully");
                        event_generation_completed = true;
                    }
                    Ok(Err(e)) => {
                        tracing::error!("Event generation task failed: {:?}", e);
                        event_generation_completed = true;
                    }
                    Err(e) => {
                        tracing::error!("Event generation task panicked: {:?}", e);
                        event_generation_completed = true;
                    }
                }
            }

            event = async {
                let mut swarm = swarm.lock().await;
                swarm.next().await
            } => {
                if let Some(event) = event {
                    match event {
                        // Gossipsub Event Handling
                        SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                            propagation_source: peer_id,
                            message_id: id,
                            message,
                        })) => {
                            tracing::info!(
                                "Got message: {} with id: {} from peer: {:?}",
                                String::from_utf8_lossy(&message.data),
                                id,
                                peer_id
                            );
                        }

                        // Handling Kademlia Events
                        SwarmEvent::Behaviour(MyBehaviourEvent::Kademlia(kad::Event::RoutingUpdated {
                            peer,
                            addresses,
                            ..
                        })) => {
                            tracing::info!("Routing table updated - Peer: {:?}, Addresses: {:?}", peer, addresses);
                        }

                        SwarmEvent::Behaviour(MyBehaviourEvent::Kademlia(kad::Event::OutboundQueryProgressed {
                            result,
                            stats,
                            ..
                        })) => {
                            match result {
                                kad::QueryResult::Bootstrap(Ok(ok)) => {
                                    tracing::info!("Bootstrap completed successfully with stats: {:?}, ok: {:?}", stats, ok);
                                }
                                kad::QueryResult::Bootstrap(Err(err)) => {
                                    tracing::error!("Bootstrap process failed: {:?}", err);
                                }
                                kad::QueryResult::GetClosestPeers(Ok(peers)) => {
                                    tracing::info!("Found closest peers: {:?}", peers);
                                }
                                kad::QueryResult::GetProviders(Ok(providers)) => {
                                    tracing::info!("Found providers: {:?}", providers);
                                }
                                kad::QueryResult::GetRecord(Ok(records)) => {
                                    tracing::info!("Found records: {:?}", records);
                                }
                                kad::QueryResult::PutRecord(Ok(put_result)) => {
                                    tracing::info!("Record put successfully: {:?}", put_result);
                                }
                                kad::QueryResult::StartProviding(Ok(providing)) => {
                                    tracing::info!("Started providing: {:?}", providing);
                                }
                                _ => {
                                    tracing::debug!("Other query result: {:?}", result);
                                }
                            }
                        }

                        // Handling Connection Established
                        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                            tracing::info!("Connected to {} via {:?}", peer_id, endpoint);
                        }

                        // Handling New Listen Address
                        SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::info!("Listening on {}", address);
                        }

                        // Handling Identify Behaviour Events
                        SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Sent { peer_id, .. })) => {
                            tracing::info!("Sent identify info to {:?}", peer_id);
                        }

                        SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received { info, .. })) => {
                            tracing::info!("Received {:?}", info);
                        }

                        _ => tracing::debug!("Other event: {:?}", event),
                    }
                }
            }
        }
    }
}
