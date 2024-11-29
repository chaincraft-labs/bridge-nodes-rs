use futures::StreamExt;
use libp2p::identity::Keypair;
use libp2p::kad::Mode;
use libp2p::{identify, kad, ping, PeerId};
use libp2p::kad::store::MemoryStore;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::select;
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
use std::collections::hash_map::DefaultHasher;
use tracing::{debug, error, info};

use crate::types::{
    NodeConfig,
    NodeBehaviour,
    CustomEvent,
    NodeError,
    NodeState,
    NodeMetrics,
    NodeBehaviourEvent,
};
use crate::utils::peer_id::{generate_keypair, read_keypair_from_file, DefaultUserDirectoryProvider};


pub struct Node {
    config: NodeConfig,
    state: NodeState,
    metrics: NodeMetrics,
    swarm: Option<Arc<Mutex<libp2p::Swarm<NodeBehaviour>>>>,
    keypair: Option<Keypair>,
}

impl CustomEvent {
    fn new() -> Result<Self, String> {
        Ok(Self {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs(),
            event_type: "ExampleEvent".to_string(),
            data: "Some event data".to_string(),
        })
    }
}

impl Node {
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

    // Setting up message ID for Gossipsub
    fn configure_message_id_fn() -> impl Fn(&gossipsub::Message) -> gossipsub::MessageId {
        |message: &gossipsub::Message| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            gossipsub::MessageId::from(s.finish().to_string())
        }
    }

    // Gossipsub Setup
    fn configure_gossipsub() -> Result<gossipsub::Config, NodeError> {
        gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10))
            .validation_mode(gossipsub::ValidationMode::Strict)
            .message_id_fn(Self::configure_message_id_fn())
            .build()
            .map_err(|e| NodeError::ConfigError(e.to_string()))
    }

    fn get_keypair(&self) -> Keypair {
        if self.config.local_test {
            // for testing
            generate_keypair(None)
        } else {
            self.keypair.clone().unwrap()
        }
    }

    // Swarm Initialization
    async fn init_swarm(&mut self) -> Result<(), NodeError> {
        let keypair = &self.get_keypair();
        let gossipsub_config = Self::configure_gossipsub()?;

        let kademlia_config = {
            let mut config = kad::Config::new(libp2p::StreamProtocol::new("/ipfs/kad/1.0.0"));
            config.set_parallelism(NonZeroUsize::new(3).unwrap());
            config.set_kbucket_size(NonZeroUsize::new(20).unwrap());
            config.set_query_timeout(Duration::from_secs(60));
            config.set_record_filtering(kad::StoreInserts::Unfiltered);
            config.set_replication_factor(NonZeroUsize::new(20).unwrap());
            config.set_periodic_bootstrap_interval(Some(Duration::from_secs(60)));
            config
        };

        let swarm = SwarmBuilder::with_existing_identity(keypair.clone())
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            ).map_err(|e| NodeError::ConfigError(e.to_string()))?
            .with_quic()
            .with_dns().map_err(|e| NodeError::ConfigError(e.to_string()))?
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
                        "chaincraft-node-rdv/1.0.0".to_string(),
                        key.public(),
                    )),
                    ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(1))),
                })
            }).map_err(|e| NodeError::ConfigError(e.to_string()))?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(5)))
            .build();

        self.swarm = Some(Arc::new(Mutex::new(swarm)));
        Ok(())
    }

    async fn init_gossipsub_topic(&mut self) -> Result<gossipsub::IdentTopic, NodeError> {
        let topic = gossipsub::IdentTopic::new("custom_events");
        let swarm = self.swarm.as_ref()
            .ok_or(NodeError::SwarmNotInitialized)?;
        let mut swarm = swarm.lock().await;

        swarm.behaviour_mut().gossipsub.subscribe(&topic)
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        info!("Subscribed to topic: {}", topic);

        Ok(topic)
    }

    async fn connect_to_bootstrap(&self) -> Result<(), NodeError> {
        if self.config.is_bootstrap_node {
            return Ok(());
        }

        let bootstrap_peer_id = PeerId::from_str(&self.config.bootstrap_peer_id.as_ref().unwrap())
            .map_err(|_| NodeError::ConfigError("Missing bootstrap peer ID".to_string()))?;

        let bootstrap_addr = if let Some(addr) = &self.config.bootstrap_address {
            if addr.parse::<std::net::IpAddr>().is_ok() {
                // format!("/ip4/{}/tcp/{}/p2p/{}", addr, self.config.bootstrap_port, bootstrap_peer_id)
                format!("/ip4/{}/udp/{}/quic-v1/p2p/{}", addr, self.config.bootstrap_port, bootstrap_peer_id)
            } else {
                // format!("/dns4/{}/tcp/{}/p2p/{}", addr, self.config.bootstrap_port, bootstrap_peer_id)
                format!("/dns4/{}/udp/{}/quic-v1/p2p/{}", addr, self.config.bootstrap_port, bootstrap_peer_id)
            }.parse::<Multiaddr>()
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

    async fn handle_event(&mut self, event: SwarmEvent<NodeBehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message_id,
                message,
            })) => {
                self.metrics.messages_received += 1;
                info!(
                    "Got message: {} with id: {} from peer: {:?}",
                    String::from_utf8_lossy(&message.data),
                    message_id,
                    propagation_source
                );
            }

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

            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                info!("Connected established to {} via {:?}", peer_id, endpoint);
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::Kademlia(kad::Event::RoutingUpdated {
                peer,
                addresses,
                ..
            })) => {
                if !&self.config.is_bootstrap_started {
                    info!("Routing table updated - Peer: {:?}, Addresses: {:?}", peer, addresses);
                    if let Some(swarm) = &self.swarm {
                        let mut _swarm: tokio::sync::MutexGuard<'_, libp2p::Swarm<NodeBehaviour>> = swarm.lock().await;
                        let behaviour = _swarm.behaviour_mut();

                        info!("Get closest peers from Kademlia: {}", peer);
                        behaviour.kademlia.get_closest_peers(peer);
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

            // ... other existing event handlers
            _ => debug!("Other event: {:?}", event),
        }
    }

    async fn init_network_listener_and_dht(&mut self) -> Result<(), NodeError> {
        let swarm = self.swarm.as_ref()
            .ok_or_else(|| NodeError::SwarmNotInitialized)?;
        let mut _swarm = swarm.lock().await;
        let behaviour = _swarm.behaviour_mut();

        behaviour.kademlia.set_mode(Some(Mode::Server));

        // let addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", self.config.port)
        //     .parse()
        //     .map_err(|e| {
        //         error!("Failed to parse address: {}", e);
        //         NodeError::MultiAddrError(e)
        //     })?;

        // _swarm.listen_on(addr)
        //     .map_err(|e| {
        //         error!("Failed to start listening: {}", e);
        //         NodeError::ListenError(e)
        //     })?;

        let quic_addr: Multiaddr = format!("/ip4/0.0.0.0/udp/{}/quic-v1", self.config.port)
            .parse()
            .map_err(|e| {
                error!("Failed to parse address: {}", e);
                NodeError::MultiAddrError(e)
            })?;
        _swarm.listen_on(quic_addr)
            .map_err(|e| {
                error!("Failed to start listening: {}", e);
                NodeError::ListenError(e)
            })?;
        Ok(())
    }

    async fn publish_blockchain_event(&mut self) -> Result<(), NodeError> {
        match &self.config.gen_msg {
            true => {
                let swarm = self.swarm.as_ref()
                    .ok_or_else(|| NodeError::SwarmNotInitialized)?;
                let swarm = swarm.clone();

                let topic = gossipsub::IdentTopic::new("custom_events");
                let topic = topic.clone();

                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(10)).await;

                        let mut swarm_lock = swarm.lock().await;
                        let behaviour = swarm_lock.behaviour_mut();
                        let event = CustomEvent::new();
                        let message = serde_json::to_string(&event).unwrap();

                        if let Err(e) = behaviour.gossipsub.publish(
                            topic.clone(),
                            message.as_bytes(),
                        ) {
                            error!("Failed to publish event: {:?}", e);
                        } else {
                            info!("Event published successfully");
                        }
                    }
                });
            }
            _ => {
                // warn!("Unauthorized event generation node");
            }
        }

        Ok(())
    }

    pub async fn run(&mut self) -> Result<(), NodeError> {
        self.init_swarm().await?;
        self.init_network_listener_and_dht().await?;
        self.connect_to_bootstrap().await?;
        self.state = NodeState::Connected;
        self.init_gossipsub_topic().await?;
        self.publish_blockchain_event().await?;

        let swarm = Arc::clone(self.swarm.as_ref()
            .expect("Swarm should be initialized"));

        loop {
            select! {
                _event = async {
                    let event = {
                        let mut locked_swarm = swarm.lock().await;
                        locked_swarm.next().await
                    };
                    event
                } => {
                    if let Some(event) = _event {
                        self.handle_event(event).await;
                    } else {
                        println!("Swarm is closed");
                    }
                }
            }
        }
    }
}