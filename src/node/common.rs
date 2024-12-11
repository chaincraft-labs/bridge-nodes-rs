use libp2p::identity::Keypair;
use libp2p::kad::Mode;
use libp2p::request_response::ProtocolSupport;
use libp2p::{identify, kad, ping, request_response, PeerId, StreamProtocol};
use libp2p::kad::store::MemoryStore;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use libp2p::{
    gossipsub,
    swarm::SwarmEvent,
    tcp,
    noise,
    yamux,
    Multiaddr,
    SwarmBuilder
};
use std::time::Duration;
use std::hash::{Hash, Hasher};
use tracing::{debug, error, info, warn};

use crate::types::{NodeConfig, NodeBehaviour, NodeError, NodeState, NodeMetrics, NodeBehaviourEvent};
use crate::utils::peer_id::{generate_keypair, read_keypair_from_file, DefaultUserDirectoryProvider};

const KEY_EXCHANGE_PROTOCOL: &str = "/key-exchange/1.0.0";

// Trait that defines common behavior for all node types
pub trait NodeCommon {
    fn get_config(&self) -> &NodeConfig;
    fn get_state(&self) -> &NodeState;
    fn get_metrics(&self) -> &NodeMetrics;
    fn get_swarm(&self) -> Option<&Arc<Mutex<libp2p::Swarm<NodeBehaviour>>>>;
    fn get_keypair(&self) -> Option<&Keypair>;
}

// Common implementations that can be shared between different node types
#[derive(Clone)]
pub struct BaseNode {
    pub config: NodeConfig,
    pub state: NodeState,
    pub metrics: NodeMetrics,
    pub swarm: Option<Arc<Mutex<libp2p::Swarm<NodeBehaviour>>>>,
    pub keypair: Option<Keypair>,
}

impl BaseNode {
    pub fn new(config: NodeConfig) -> Self {
        let user_dir_provider = DefaultUserDirectoryProvider;

        Self {
            config,
            state: NodeState::Starting,
            metrics: NodeMetrics::default(),
            swarm: None,
            keypair: read_keypair_from_file(&user_dir_provider).ok(),
        }
    }

    // Common network setup methods
    pub fn configure_message_id_fn() -> impl Fn(&gossipsub::Message) -> gossipsub::MessageId {
        |message: &gossipsub::Message| {
            let mut s = std::collections::hash_map::DefaultHasher::new();
            message.data.hash(&mut s);
            gossipsub::MessageId::from(s.finish().to_string())
        }
    }

    pub fn configure_gossipsub() -> Result<gossipsub::Config, NodeError> {
        gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .message_id_fn(Self::configure_message_id_fn())
            .build()
            .map_err(|e| NodeError::ConfigError(e.to_string()))
    }

    pub fn configure_kademlia() -> kad::Config {
        let mut config = kad::Config::new(libp2p::StreamProtocol::new("/ipfs/kad/1.0.0"));
        config.set_parallelism(NonZeroUsize::new(3).unwrap());
        config.set_kbucket_size(NonZeroUsize::new(20).unwrap());
        config.set_query_timeout(Duration::from_secs(60));
        config.set_record_filtering(kad::StoreInserts::Unfiltered);
        config.set_replication_factor(NonZeroUsize::new(20).unwrap());
        config.set_periodic_bootstrap_interval(Some(Duration::from_secs(60)));
        config
    }

    pub fn get_keypair_for_network(&self) -> Keypair {
        if self.config.local_test {
            generate_keypair(None)
        } else {
            // If we don't have a keypair from file, generate a new one
            self.keypair.clone().unwrap_or_else(|| {
                warn!("No keypair found in file, generating new one");
                generate_keypair(None)
            })
        }
        // if self.config.local_test {
        //     generate_keypair(None)
        // } else {
        //     self.keypair.clone().unwrap()
        // }
    }

    // Common initialization methods
    pub async fn init_swarm(&mut self) -> Result<(), NodeError> {
        let keypair = &self.get_keypair_for_network();
        let gossipsub_config = Self::configure_gossipsub()?;
        let kademlia_config = Self::configure_kademlia();
        let request_response_config = request_response::Config::default()
            .with_request_timeout(Duration::from_secs(30));

        let swarm = SwarmBuilder::with_existing_identity(keypair.clone())
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| NodeError::ConfigError(e.to_string()))?
            .with_quic()
            .with_dns()
            .map_err(|e| NodeError::ConfigError(e.to_string()))?
            .with_behaviour(|key| {
                Ok(NodeBehaviour {
                    gossipsub: gossipsub::Behaviour::new(
                        gossipsub::MessageAuthenticity::Signed(key.clone()),
                        gossipsub_config,
                    )
                    .map_err(|e| NodeError::ConfigError(e.to_string()))?,
                    kademlia: kad::Behaviour::with_config(
                        key.public().to_peer_id(),
                        MemoryStore::new(key.public().to_peer_id()),
                        kademlia_config,
                    ),
                    identify: identify::Behaviour::new(identify::Config::new(
                        "chaincraft-p2p/1.0.0".to_string(),
                        key.public(),
                    )),
                    ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(1))),
                    key_exchange: request_response::json::Behaviour::new(
                        [(StreamProtocol::new(KEY_EXCHANGE_PROTOCOL), ProtocolSupport::Full)],
                        request_response_config,
                    ),
                })
            })
            .map_err(|e| NodeError::ConfigError(e.to_string()))?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(5)))
            .build();

        self.swarm = Some(Arc::new(Mutex::new(swarm)));
        Ok(())
    }

    pub async fn connect_to_bootstrap(&self) -> Result<(), NodeError> {
        if self.config.is_bootstrap_node {
            return Ok(());
        }

        let bootstrap_peer_id = PeerId::from_str(&self.config.bootstrap_peer_id.as_ref().unwrap())
            .map_err(|_| NodeError::ConfigError("Missing bootstrap peer ID".to_string()))?;

        let bootstrap_addr = if let Some(addr) = &self.config.bootstrap_address {
            if addr.parse::<std::net::IpAddr>().is_ok() {
                format!(
                    "/ip4/{}/udp/{}/quic-v1/p2p/{}",
                    addr,
                    self.config.bootstrap_port,
                    bootstrap_peer_id
                )
            } else {
                format!(
                    "/dns4/{}/udp/{}/quic-v1/p2p/{}",
                    addr,
                    self.config.bootstrap_port,
                    bootstrap_peer_id
                )
            }
            .parse::<Multiaddr>()
            .map_err(|e| NodeError::ConfigError(e.to_string()))?
        } else {
            return Err(NodeError::ConfigError("Missing bootstrap address".to_string()));
        };

        if let Some(swarm) = &self.swarm {
            let mut swarm = swarm.lock().await;
            info!("Add Bootstrap peer ID: {} Addr: {}", bootstrap_peer_id, bootstrap_addr);
            swarm.behaviour_mut().kademlia.add_address(&bootstrap_peer_id, bootstrap_addr);
        }

        Ok(())
    }

    // Common network setup methods
    pub async fn init_network_listener(&mut self) -> Result<(), NodeError> {
        let swarm = self.swarm.as_ref()
            .ok_or_else(|| NodeError::SwarmNotInitialized)?;
        let mut swarm = swarm.lock().await;
        let behaviour = swarm.behaviour_mut();

        behaviour.kademlia.set_mode(Some(Mode::Server));

        let quic_addr: Multiaddr = format!("/ip4/{}/udp/{}/quic-v1", self.config.listen_address, self.config.port)
            .parse()
            .map_err(|e| {
                error!("Failed to parse address: {}", e);
                NodeError::MultiAddrError(e)
            })?;

        swarm.listen_on(quic_addr)
            .map_err(|e| {
                error!("Failed to start listening: {}", e);
                NodeError::ListenError(e)
            })?;

        Ok(())
    }

    pub async fn handle_common_events(&mut self, event: SwarmEvent<NodeBehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(NodeBehaviourEvent::Kademlia(kad::Event::OutboundQueryProgressed {
                result,
                stats,
                ..
            })) => {
                match result {
                    kad::QueryResult::Bootstrap(Ok(ok)) => {
                        info!("Bootstrap succeeded with stats: {:?}, ok: {:?}", stats, ok);
                    }
                    kad::QueryResult::Bootstrap(Err(err)) => {
                        error!("Bootstrap process failed: {:?}", err);
                    }
                    kad::QueryResult::GetClosestPeers(Ok(peers)) => {
                        info!("Found closest peers: {:?}", peers.peers);
                    }
                    // kad::QueryResult::GetProviders(Ok(providers)) => {
                    //     info!("Found providers: {:?}", providers);
                    // }
                    // kad::QueryResult::GetRecord(Ok(records)) => {
                    //     info!("Found records: {:?}", records);
                    // }
                    // kad::QueryResult::PutRecord(Ok(put_result)) => {
                    //     info!("Record put successfully: {:?}", put_result);
                    // }
                    // kad::QueryResult::StartProviding(Ok(providing)) => {
                    //     info!("Started providing: {:?}", providing);
                    // }
                    _ => {
                        debug!("Other query result: {:?}", result);
                    }
                }
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::Kademlia(kad::Event::RoutingUpdated {
                peer,
                addresses,
                ..
            })) => {
                if !self.config.is_bootstrap_started {
                    info!("Routing table updated - Peer: {:?}, Addresses: {:?}", peer, addresses);
                    if let Some(swarm) = &self.swarm {
                        let mut swarm = swarm.lock().await;
                        info!("Get closest peers from Kademlia: {}", peer);
                        swarm.behaviour_mut().kademlia.get_closest_peers(peer);
                    }
                    self.config.is_bootstrap_started = true;
                }
            }

            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {}", address);
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::Identify(identify::Event::Sent { peer_id, .. })) => {
                info!("Sent identify info to {:?}", peer_id);
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::Identify(identify::Event::Received { info, .. })) => {
                // Receive information every 5 minutes
                info!("Received {:?}", info);
                if let Some(swarm) = &self.swarm {
                    let mut _swarm: tokio::sync::MutexGuard<'_, libp2p::Swarm<NodeBehaviour>> = swarm.lock().await;
                    let behaviour = _swarm.behaviour_mut();
                    let peer_id = info.public_key.clone().to_peer_id();

                    for addr in info.listen_addrs {
                        let addr = format!("{}/p2p/{}", addr, peer_id).parse::<Multiaddr>().unwrap();
                        info!("Add peer ID to Kademlia: {}, Address: {}", peer_id, addr);
                        behaviour.kademlia.add_address(&peer_id, addr);
                    }

                    info!("Add peer ID to Gossipsub: {}", peer_id);
                    behaviour.gossipsub.add_explicit_peer(&peer_id);
                }
            }

            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                info!("Connected established to {} via {:?}", peer_id, endpoint);

                if let Some(swarm) = &self.swarm {
                    let mut _swarm: tokio::sync::MutexGuard<'_, libp2p::Swarm<NodeBehaviour>> = swarm.lock().await;
                    let behaviour = _swarm.behaviour_mut();
                    let remote_addr = endpoint.get_remote_address().clone();

                    let addr = format!("{}/p2p/{}", remote_addr, peer_id).parse::<Multiaddr>().unwrap();
                    info!("Add peer ID to Kademlia: {}, Address: {}", peer_id, addr);
                    behaviour.kademlia.add_address(&peer_id, addr);

                    info!("Add peer ID to Gossipsub: {}", peer_id);
                    behaviour.gossipsub.add_explicit_peer(&peer_id);
                }
            }

            // Add other common event handling logic here...
            _ => debug!("Other event: {:?}", event),
        }
    }
}