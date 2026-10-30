use libp2p::identity::Keypair;
use libp2p::kad::store::MemoryStore;
use libp2p::kad::Mode;
use libp2p::request_response::ProtocolSupport;
use libp2p::{gossipsub, swarm::SwarmEvent, Multiaddr, SwarmBuilder};
use libp2p::{identify, kad, ping, request_response, PeerId, StreamProtocol};
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::types::{
    NodeBehaviour, NodeBehaviourEvent, NodeConfig, NodeError, NodeMetrics, NodeState,
};
use crate::utils::peer_id::{
    generate_keypair, read_keypair_from_file, DefaultUserDirectoryProvider,
};

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
        Self {
            config,
            state: NodeState::Starting,
            metrics: NodeMetrics::default(),
            swarm: None,
            keypair: None,
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

    pub fn get_keypair_for_network(&mut self) -> Keypair {
        if self.config.local_test {
            generate_keypair(None)
        } else {
            let user_dir_provider = DefaultUserDirectoryProvider;
            self.keypair = read_keypair_from_file(&user_dir_provider).ok();

            self.keypair.clone().unwrap_or_else(|| {
                warn!("No keypair found in file, generating new one");
                generate_keypair(None)
            })
        }
    }

    // Common initialization methods
    pub async fn init_swarm(&mut self) -> Result<(), NodeError> {
        let keypair = &self.get_keypair_for_network();
        self.keypair = Some(keypair.clone());

        let gossipsub_config = Self::configure_gossipsub()?;
        let kademlia_config = Self::configure_kademlia();
        let request_response_config =
            request_response::Config::default().with_request_timeout(Duration::from_secs(30));

        let swarm = SwarmBuilder::with_existing_identity(keypair.clone())
            .with_tokio()
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
                    ping: ping::Behaviour::new(
                        ping::Config::new().with_interval(Duration::from_secs(1)),
                    ),
                    key_exchange: request_response::json::Behaviour::new(
                        [(
                            StreamProtocol::new(KEY_EXCHANGE_PROTOCOL),
                            ProtocolSupport::Full,
                        )],
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

        // Only for validator nodes
        let bootstrap_peer_id =
            PeerId::from_str(self.config.bootstrap_peer_id.as_ref().unwrap())
                .map_err(|_| NodeError::ConfigError("Missing bootstrap peer ID".to_string()))?;

        let bootstrap_addr = if let Some(addr) = &self.config.bootstrap_address {
            if addr.parse::<std::net::IpAddr>().is_ok() {
                format!(
                    "/ip4/{}/udp/{}/quic-v1/p2p/{}",
                    addr, self.config.bootstrap_port, bootstrap_peer_id
                )
            } else {
                format!(
                    "/dns4/{}/udp/{}/quic-v1/p2p/{}",
                    addr, self.config.bootstrap_port, bootstrap_peer_id
                )
            }
            .parse::<Multiaddr>()
            .map_err(|e| NodeError::ConfigError(e.to_string()))?
        } else {
            return Err(NodeError::ConfigError(
                "Missing bootstrap address".to_string(),
            ));
        };

        if let Some(swarm) = &self.swarm {
            let mut swarm = swarm.lock().await;
            info!(
                "Add Bootstrap peer ID: {} Addr: {}",
                bootstrap_peer_id, bootstrap_addr
            );
            swarm
                .behaviour_mut()
                .kademlia
                .add_address(&bootstrap_peer_id, bootstrap_addr);
        }

        Ok(())
    }

    // Common network setup methods
    pub async fn init_network_listener(&mut self) -> Result<(), NodeError> {
        let swarm = self
            .swarm
            .as_ref()
            .ok_or_else(|| NodeError::SwarmNotInitialized)?;
        let mut swarm = swarm.lock().await;
        let behaviour = swarm.behaviour_mut();

        behaviour.kademlia.set_mode(Some(Mode::Server));

        let quic_addr: Multiaddr = format!(
            "/ip4/{}/udp/{}/quic-v1",
            self.config.listen_address, self.config.port
        )
        .parse()
        .map_err(|e| {
            error!("Failed to parse address: {}", e);
            NodeError::MultiAddrError(e)
        })?;

        swarm.listen_on(quic_addr).map_err(|e| {
            error!("Failed to start listening: {}", e);
            NodeError::ListenError(e)
        })?;

        Ok(())
    }

    // Takes the swarm from the caller, which owns it (relayer) or holds its lock
    // (validator): this method never locks `self.swarm`.
    pub async fn handle_common_events(
        &mut self,
        swarm: &mut libp2p::Swarm<NodeBehaviour>,
        event: SwarmEvent<NodeBehaviourEvent>,
    ) {
        match event {
            SwarmEvent::Behaviour(NodeBehaviourEvent::Kademlia(
                kad::Event::OutboundQueryProgressed { result, stats, .. },
            )) => {
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
                    info!(
                        "Routing table updated - Peer: {:?}, Addresses: {:?}",
                        peer, addresses
                    );
                    info!("Get closest peers from Kademlia: {}", peer);
                    swarm.behaviour_mut().kademlia.get_closest_peers(peer);
                    self.config.is_bootstrap_started = true;
                }
            }

            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {}", address);
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::Identify(identify::Event::Sent {
                peer_id,
                ..
            })) => {
                info!("Sent identify info to {:?}", peer_id);
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::Identify(identify::Event::Received {
                info,
                ..
            })) => {
                // Receive information every 5 minutes
                info!("Received {:?}", info);
                let behaviour = swarm.behaviour_mut();
                let peer_id = info.public_key.clone().to_peer_id();

                for addr in info.listen_addrs {
                    let addr = format!("{}/p2p/{}", addr, peer_id)
                        .parse::<Multiaddr>()
                        .unwrap();
                    info!("Add peer ID to Kademlia: {}, Address: {}", peer_id, addr);
                    behaviour.kademlia.add_address(&peer_id, addr);
                }

                info!("Add peer ID to Gossipsub: {}", peer_id);
                behaviour.gossipsub.add_explicit_peer(&peer_id);
            }

            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                info!("Connected established to {} via {:?}", peer_id, endpoint);

                let behaviour = swarm.behaviour_mut();
                let remote_addr = endpoint.get_remote_address().clone();
                let addr = format!("{}/p2p/{}", remote_addr, peer_id)
                    .parse::<Multiaddr>()
                    .unwrap();

                info!("Add peer ID to Kademlia: {}, Address: {}", peer_id, addr);
                behaviour.kademlia.add_address(&peer_id, addr);

                info!("Add peer ID to Gossipsub: {}", peer_id);
                behaviour.gossipsub.add_explicit_peer(&peer_id);
            }

            // Add other common event handling logic here...
            _ => debug!("Other event: {:?}", event),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unauthorized_peer_and_peer_error_messages() {
        assert_eq!(
            NodeError::UnauthorizedPeer("12D3KooWX".to_string()).to_string(),
            "Peer 12D3KooWX unauthorized"
        );
        assert_eq!(
            NodeError::PeerError("Peer 12D3KooWX not connected".to_string()).to_string(),
            "Peer error: Peer 12D3KooWX not connected"
        );
    }
}

/// Two swarms talking over QUIC on loopback, in process, for the tests of
/// the node's event handlers. Every node is built with `local_test`: a
/// random identity, and no keypair file read or written.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use futures::StreamExt;
    use libp2p::multiaddr::Protocol;
    use libp2p::Swarm;

    pub fn test_config(is_bootstrap_node: bool) -> NodeConfig {
        NodeConfig {
            port: 0,
            bootstrap_address: None,
            bootstrap_port: 0,
            bootstrap_peer_id: None,
            is_bootstrap_node,
            is_bootstrap_started: false,
            gen_msg: false,
            listen_address: "127.0.0.1".to_string(),
            local_test: true,
            rpc_address: None,
        }
    }

    /// A bare swarm, owned by the test, with the node's behaviours.
    pub async fn bare_swarm() -> Swarm<NodeBehaviour> {
        let mut node = BaseNode::new(test_config(false));
        node.init_swarm().await.unwrap();
        let swarm = node.swarm.take().unwrap();
        Arc::try_unwrap(swarm)
            .unwrap_or_else(|_| panic!("a new swarm is not shared"))
            .into_inner()
    }

    /// Listens on a new loopback port and returns the address once bound.
    pub async fn listen(swarm: &mut Swarm<NodeBehaviour>) -> Multiaddr {
        let listener = swarm
            .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
            .unwrap();
        loop {
            if let SwarmEvent::NewListenAddr {
                listener_id,
                address,
            } = swarm.select_next_some().await
            {
                if listener_id == listener {
                    return address;
                }
            }
        }
    }

    /// The addresses Kademlia holds for `peer`, without their `/p2p` part.
    pub fn kademlia_addresses(swarm: &mut Swarm<NodeBehaviour>, peer: &PeerId) -> Vec<Multiaddr> {
        let mut addresses = Vec::new();
        for bucket in swarm.behaviour_mut().kademlia.kbuckets() {
            for entry in bucket.iter() {
                if entry.node.key.preimage() == peer {
                    addresses.extend(entry.node.value.iter().map(|address| {
                        address
                            .iter()
                            .filter(|protocol| !matches!(protocol, Protocol::P2p(_)))
                            .collect::<Multiaddr>()
                    }));
                }
            }
        }
        addresses
    }

    type Logs = Arc<std::sync::Mutex<Vec<String>>>;

    std::thread_local! {
        static SINK: std::cell::RefCell<Option<Logs>> = const { std::cell::RefCell::new(None) };
    }

    /// Stops the capture of this thread when dropped.
    pub struct CaptureGuard;

    impl Drop for CaptureGuard {
        fn drop(&mut self) {
            SINK.with(|sink| sink.borrow_mut().take());
        }
    }

    /// The messages of the events logged on this thread while the returned
    /// guard lives. The tests run on tokio's current-thread runtime, so the
    /// node's handlers log on the test's thread.
    ///
    /// One global subscriber serves every test: a subscriber set per thread
    /// would miss the call sites another thread reached first, since tracing
    /// caches whether a call site is enabled across threads.
    pub fn capture_logs() -> (Logs, CaptureGuard) {
        use tracing_subscriber::layer::SubscriberExt;
        static GLOBAL: std::sync::Once = std::sync::Once::new();

        GLOBAL.call_once(|| {
            tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Capture))
                .expect("no other global subscriber in the tests");
        });
        let logs = Logs::default();
        SINK.with(|sink| *sink.borrow_mut() = Some(Arc::clone(&logs)));
        (logs, CaptureGuard)
    }

    pub fn logged(logs: &std::sync::Mutex<Vec<String>>, prefix: &str) -> bool {
        logs.lock()
            .unwrap()
            .iter()
            .any(|message| message.starts_with(prefix))
    }

    struct Capture;

    struct Message(String);

    impl tracing::field::Visit for Message {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{:?}", value);
            }
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            // Only this crate's info and above, as the smoke test reads them.
            let metadata = event.metadata();
            if !metadata.target().starts_with("validator_node")
                || *metadata.level() > tracing::Level::INFO
            {
                return;
            }
            SINK.with(|sink| {
                if let Some(logs) = sink.borrow().as_ref() {
                    let mut message = Message(String::new());
                    event.record(&mut message);
                    logs.lock().unwrap().push(message.0);
                }
            });
        }
    }
}
