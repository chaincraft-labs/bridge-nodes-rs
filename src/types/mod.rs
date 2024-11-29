use libp2p::{
    gossipsub, identify, kad::{self, store::MemoryStore}, ping, swarm::NetworkBehaviour, Multiaddr, PeerId
};
use serde::{Deserialize, Serialize};
use std::time::Duration;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub data: String,
}

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub bootstrap_address: Option<String>,
    pub bootstrap_port: u16,
    pub bootstrap_peer_id: Option<String>,
    pub is_bootstrap_node: bool,
    pub is_bootstrap_started: bool,
    pub gen_msg: bool,
    pub listen_address: String,
    pub port: u16,
    pub local_test: bool,
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeState {
    Starting,
    Bootstrapping,
    Connected,
    Disconnected,
    Error(String),
}


#[derive(NetworkBehaviour)]
#[behaviour(event_process = false)]
#[behaviour(out_event = "NodeBehaviourEvent")]
pub struct NodeBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<MemoryStore>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
}

#[derive(Debug)]
pub enum NodeBehaviourEvent {
    Gossipsub(gossipsub::Event),
    Kademlia(kad::Event),
    Identify(identify::Event),
    Ping(ping::Event),
}


impl From<gossipsub::Event> for NodeBehaviourEvent {
    fn from(event: gossipsub::Event) -> Self {
        NodeBehaviourEvent::Gossipsub(event)
    }
}

impl From<kad::Event> for NodeBehaviourEvent {
    fn from(event: kad::Event) -> Self {
        NodeBehaviourEvent::Kademlia(event)
    }
}

impl From<identify::Event> for NodeBehaviourEvent {
    fn from(event: identify::Event) -> Self {
        NodeBehaviourEvent::Identify(event)
    }
}

impl From<ping::Event> for NodeBehaviourEvent {
    fn from(event: ping::Event) -> Self {
        NodeBehaviourEvent::Ping(event)
    }
}


#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Peer error: {0}")]
    PeerError(String),

    #[error("Bootstrap error: {0}")]
    BootstrapError(String),

    #[error("Swarm not initialized")]
    SwarmNotInitialized,

    #[error("Gossipsub error: {0}")]
    GossipsubError(String),

    #[error("Gossipsub topic error: {0}")]
    TopicNotInitialized(String),

    #[error("Multiaddr parsing error: {0}")]
    MultiAddrError(#[from] libp2p::multiaddr::Error),

    #[error("Listen error: {0}")]
    ListenError(#[from] libp2p::TransportError<std::io::Error>),

    #[error("Failed to create event: {0}")]
    EventCreationError(String),

    #[error("Failed to serialize event: {0}")]
    SerializationError(String),

    #[error("Failed to publish event: {0}")]
    PublishError(String),

    #[error("Task join error: {0}")]
    TaskJoinError(#[from] tokio::task::JoinError),

    #[error("Event task error: {0}")]
    TaskError(String),

    #[error("Dial error: {0}")]
    DialError(String),
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    Event(CustomEvent),
    Control(ControlMessage),
    Status(StatusMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    Connect(PeerId),
    Disconnect(PeerId),
    Subscribe(String),
    Unsubscribe(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusMessage {
    pub peer_id: PeerId,
    pub uptime: Duration,
    pub connected_peers: Vec<PeerId>,
    pub state: NodeState,
}


#[derive(Debug, Clone, Default)]
pub struct NodeMetrics {
    pub messages_received: u64,
    pub messages_sent: u64,
    pub connected_peers_count: u32,
    pub active_subscriptions: Vec<String>,
    pub last_event_timestamp: Option<u64>,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub storage_path: String,
    pub max_events: usize,
    pub retention_period: Duration,
}


#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub peer_id: PeerId,
    pub addresses: Vec<Multiaddr>,
    pub last_seen: Option<u64>,
    pub connection_status: PeerConnectionStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PeerConnectionStatus {
    Connected,
    Disconnected,
    Banned,
    Unknown,
}

