use futures::StreamExt;
use libp2p::allow_block_list::{self, AllowedPeers};
use libp2p::identity::Keypair;
use libp2p::{gossipsub, PeerId, Swarm};
use libp2p::{
    request_response,
    swarm::{ListenError, SwarmEvent},
};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{error, info, warn};

use super::authorized::{tree_store, AuthorizedNodes, AuthorizedPeers};
use super::common::{BaseNode, NodeCommon};
use crate::rpc::start_rpc_server;
use crate::utils::peer_id::DefaultUserDirectoryProvider;
use crate::{
    types::{rpc_bind_address, NodeBehaviourEvent, NodeConfig, NodeError, NodeMetrics, NodeState},
    NodeBehaviour,
};
use crate::{PartialKeyRequest, PartialKeyResponse};

/// Commands the RPC server may queue before the run loop takes them.
const COMMAND_CHANNEL_CAPACITY: usize = 32;

/// What the RPC server asks of the relayer's run loop. Each command carries
/// the channel its answer goes back on.
#[derive(Debug)]
pub enum RelayerCommand {
    DistributeKeys {
        peers: Vec<PeerId>,
        reply: oneshot::Sender<Result<(), NodeError>>,
    },
    AuthorizeNodes {
        peers: Vec<PeerId>,
        reply: oneshot::Sender<Result<AuthorizedNodes, NodeError>>,
    },
    RevokeNodes {
        peers: Vec<PeerId>,
        reply: oneshot::Sender<Result<AuthorizedNodes, NodeError>>,
    },
    AuthorizedNodes {
        reply: oneshot::Sender<AuthorizedNodes>,
    },
}

/// The sending half the RPC server holds.
pub type CommandSender = mpsc::Sender<RelayerCommand>;

/// Sends a key request to each peer, in order, its index being its position
/// in `peers`. Stops at the first peer that is not connected.
pub fn distribute_keys(
    swarm: &mut Swarm<NodeBehaviour>,
    peers: &[PeerId],
) -> Result<(), NodeError> {
    for (index, peer_id) in peers.iter().enumerate() {
        if !swarm.is_connected(peer_id) {
            return Err(NodeError::PeerError(format!(
                "Peer {} not connected. Ensure peer is connected before distributing keys",
                peer_id
            )));
        }

        let request = PartialKeyRequest { node_index: index };

        swarm
            .behaviour_mut()
            .key_exchange
            .send_request(peer_id, request);
    }

    Ok(())
}

fn allow_list(
    swarm: &mut Swarm<NodeBehaviour>,
) -> Option<&mut allow_block_list::Behaviour<AllowedPeers>> {
    swarm.behaviour_mut().allow_list.as_mut()
}

/// Allows every authorized peer on the swarm. The relayer refuses to run
/// without its allow list: it would accept any peer.
fn allow_authorized_peers(
    swarm: &mut Swarm<NodeBehaviour>,
    authorized: &AuthorizedPeers,
) -> Result<(), NodeError> {
    let allow_list = allow_list(swarm).ok_or_else(|| {
        NodeError::ConfigError("The relayer's allow list is disabled".to_string())
    })?;
    for peer in authorized.peers() {
        allow_list.allow_peer(*peer);
    }
    Ok(())
}

/// Authorizes `peers` and lets them connect.
pub fn authorize_nodes(
    swarm: &mut Swarm<NodeBehaviour>,
    authorized: &mut AuthorizedPeers,
    peers: &[PeerId],
) -> Result<AuthorizedNodes, NodeError> {
    for peer in authorized.authorize(peers)? {
        if let Some(allow_list) = allow_list(swarm) {
            allow_list.allow_peer(peer);
        }
        info!("Authorized peer {}", peer);
    }
    Ok(authorized.state())
}

/// Revokes `peers`: they may no longer connect, and their connections are
/// closed.
pub fn revoke_nodes(
    swarm: &mut Swarm<NodeBehaviour>,
    authorized: &mut AuthorizedPeers,
    peers: &[PeerId],
) -> Result<AuthorizedNodes, NodeError> {
    for peer in authorized.revoke(peers)? {
        // Closes the peer's connections, too.
        if let Some(allow_list) = allow_list(swarm) {
            allow_list.disallow_peer(peer);
        }
        info!("Revoked peer {}: its connections are closed", peer);
    }
    Ok(authorized.state())
}

fn handle_command(
    swarm: &mut Swarm<NodeBehaviour>,
    authorized: &mut AuthorizedPeers,
    command: RelayerCommand,
) {
    let delivered = match command {
        RelayerCommand::DistributeKeys { peers, reply } => {
            reply.send(distribute_keys(swarm, &peers)).is_ok()
        }
        RelayerCommand::AuthorizeNodes { peers, reply } => reply
            .send(authorize_nodes(swarm, authorized, &peers))
            .is_ok(),
        RelayerCommand::RevokeNodes { peers, reply } => {
            reply.send(revoke_nodes(swarm, authorized, &peers)).is_ok()
        }
        RelayerCommand::AuthorizedNodes { reply } => reply.send(authorized.state()).is_ok(),
    };
    if !delivered {
        warn!("The RPC caller left before the relayer answered");
    }
}

pub struct RelayerNode {
    base: BaseNode,
}

impl NodeCommon for RelayerNode {
    fn get_config(&self) -> &NodeConfig {
        &self.base.config
    }

    fn get_state(&self) -> &NodeState {
        &self.base.state
    }

    fn get_metrics(&self) -> &NodeMetrics {
        &self.base.metrics
    }

    fn get_swarm(&self) -> Option<&Arc<Mutex<libp2p::Swarm<NodeBehaviour>>>> {
        self.base.swarm.as_ref()
    }

    fn get_keypair(&self) -> Option<&Keypair> {
        self.base.keypair.as_ref()
    }
}

impl RelayerNode {
    pub fn new(config: NodeConfig) -> Self {
        Self {
            base: BaseNode::new(config),
        }
    }

    async fn init_gossipsub_topic(&mut self) -> Result<gossipsub::IdentTopic, NodeError> {
        let topic = gossipsub::IdentTopic::new("custom_events");

        let swarm = self
            .base
            .swarm
            .as_ref()
            .ok_or(NodeError::SwarmNotInitialized)?;
        let mut swarm = swarm.lock().await;

        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&topic)
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        info!("Subscribed to topic: {}", topic);

        Ok(topic)
    }

    // The run loop owns the swarm, so no lock is held while it waits.
    fn take_swarm(&mut self) -> Result<Swarm<NodeBehaviour>, NodeError> {
        let swarm = self
            .base
            .swarm
            .take()
            .ok_or(NodeError::SwarmNotInitialized)?;
        Arc::try_unwrap(swarm)
            .map(Mutex::into_inner)
            .map_err(|swarm| {
                self.base.swarm = Some(swarm);
                NodeError::ConfigError("The relayer's swarm is shared".to_string())
            })
    }

    async fn handle_relayer_events(
        &mut self,
        swarm: &mut Swarm<NodeBehaviour>,
        event: SwarmEvent<NodeBehaviourEvent>,
    ) {
        match event {
            SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message_id,
                message,
            })) => {
                self.base.metrics.messages_received += 1;
                info!(
                    "Got message: {} with id: {} from peer: {:?}",
                    String::from_utf8_lossy(&message.data),
                    message_id,
                    propagation_source
                );
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(event)) => {
                match event {
                    request_response::Event::Message { peer, message, .. } => match message {
                        request_response::Message::Request {
                            request_id,
                            request,
                            channel,
                        } => {
                            info!(
                                "Relayer received request from {} with id {} for index {}",
                                peer, request_id, request.node_index
                            );
                            let response = PartialKeyResponse {
                                key_data: vec![],
                                node_index: request.node_index,
                            };
                            let _ = swarm
                                .behaviour_mut()
                                .key_exchange
                                .send_response(channel, response);
                        }
                        // Relayer should not receive responses
                        request_response::Message::Response {
                            request_id,
                            response,
                        } => {
                            info!(
                                "Relayer received response from {} with id {} response {:?}",
                                peer, request_id, response
                            );
                        }
                    },
                    request_response::Event::OutboundFailure { peer, error, .. } => {
                        error!(
                            "OutboundFailure => Failed to send key to validator {}: {:?}",
                            peer, error
                        );
                        // Error while sending message
                        // Retry here
                    }
                    request_response::Event::InboundFailure { .. } => {
                        // Ignore because the relayer should not receive requests
                    }
                    request_response::Event::ResponseSent {
                        peer, request_id, ..
                    } => {
                        info!(
                            "Successfully sent key to validator {}, request_id: {}",
                            peer, request_id
                        );
                    }
                }
            }

            SwarmEvent::IncomingConnectionError {
                peer_id: Some(peer_id),
                send_back_addr,
                error: ListenError::Denied { cause },
                ..
            } if cause
                .downcast_ref::<allow_block_list::NotAllowed>()
                .is_some() =>
            {
                warn!(
                    "Denied a connection from unauthorized peer {} at {}",
                    peer_id, send_back_addr
                );
            }

            SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
                info!("Connection closed with {}: {:?}", peer_id, cause);
            }

            _ => {
                // Handle common events through the base implementation
                self.base.handle_common_events(swarm, event).await;
            }
        }
    }

    /// Waits on swarm events and on RPC commands at once, so a command is
    /// served at once even when the swarm is idle. Returns an error when the
    /// command channel closes, which means the RPC server is gone.
    async fn run_loop(
        &mut self,
        swarm: &mut Swarm<NodeBehaviour>,
        authorized: &mut AuthorizedPeers,
        commands: &mut mpsc::Receiver<RelayerCommand>,
    ) -> Result<(), NodeError> {
        while self.base.state != NodeState::Disconnected {
            tokio::select! {
                event = swarm.select_next_some() => {
                    self.handle_relayer_events(swarm, event).await;
                }
                command = commands.recv() => match command {
                    Some(command) => handle_command(swarm, authorized, command),
                    None => {
                        return Err(NodeError::TaskError(
                            "The RPC command channel closed".to_string(),
                        ))
                    }
                },
            }
        }

        Ok(())
    }

    pub async fn run(&mut self) -> Result<(), NodeError> {
        info!("🌟 Relayer node");
        self.base.init_swarm().await?;
        let mut authorized = self.load_authorized_peers()?;

        self.base.init_network_listener().await?;
        self.base.state = NodeState::Connected;
        self.init_gossipsub_topic().await?;

        let mut swarm = self.take_swarm()?;
        allow_authorized_peers(&mut swarm, &authorized)?;
        let (commands_tx, mut commands_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        // Kept until the loop ends: dropping the handle stops the server.
        let (_rpc_handle, _) =
            start_rpc_server(commands_tx, rpc_bind_address(self.base.config.rpc_address)).await?;

        self.run_loop(&mut swarm, &mut authorized, &mut commands_rx)
            .await
    }

    // Before the node listens: a stored list it cannot trust stops it.
    fn load_authorized_peers(&self) -> Result<AuthorizedPeers, NodeError> {
        let keypair = self
            .base
            .keypair
            .clone()
            .ok_or(NodeError::SwarmNotInitialized)?;
        let store = tree_store(self.base.config.local_test, &DefaultUserDirectoryProvider)?;
        let authorized = AuthorizedPeers::load(keypair, store)?;
        let state = authorized.state();
        info!(
            "Authorized peers: {} (root {}, timestamp {})",
            state.peer_ids.len(),
            state.root,
            state.timestamp
        );
        Ok(authorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::common::test_support::{
        bare_swarm, capture_logs, kademlia_addresses, listen, logged, test_config,
    };
    use crate::utils::peer_id::generate_keypair;
    use libp2p::Multiaddr;
    use std::time::Duration;

    // An idle relayer: `--local-test` gives it a random identity and leaves
    // the keypair file alone, and it listens nowhere, so its swarm never
    // produces an event.
    async fn idle_relayer() -> RelayerNode {
        let mut node = RelayerNode::new(test_config(true));
        node.base.init_swarm().await.unwrap();
        node
    }

    fn random_peer() -> PeerId {
        generate_keypair(None).public().to_peer_id()
    }

    fn not_connected_message(peer: &PeerId) -> String {
        format!(
            "Peer {} not connected. Ensure peer is connected before distributing keys",
            peer
        )
    }

    #[tokio::test]
    async fn test_distribute_keys_refuses_a_peer_that_is_not_connected() {
        let mut swarm = idle_relayer().await.take_swarm().unwrap();
        let peer = random_peer();

        match distribute_keys(&mut swarm, &[peer]) {
            Err(NodeError::PeerError(message)) => {
                assert_eq!(message, not_connected_message(&peer))
            }
            other => panic!("expected PeerError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_take_swarm_leaves_the_node_without_a_swarm() {
        let mut node = idle_relayer().await;

        assert!(node.take_swarm().is_ok());
        assert!(node.get_swarm().is_none());
        assert!(matches!(
            node.take_swarm(),
            Err(NodeError::SwarmNotInitialized)
        ));
    }

    #[tokio::test]
    async fn test_take_swarm_refuses_a_shared_swarm_and_keeps_it() {
        let mut node = idle_relayer().await;
        let _other_holder = Arc::clone(node.get_swarm().unwrap());

        assert!(matches!(node.take_swarm(), Err(NodeError::ConfigError(_))));
        assert!(node.get_swarm().is_some());
    }

    // The run loop of an idle relayer, fed by a command channel.
    fn spawn_idle_loop(
        mut node: RelayerNode,
    ) -> (
        CommandSender,
        tokio::task::JoinHandle<Result<(), NodeError>>,
    ) {
        let (commands_tx, mut commands_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let task = tokio::spawn(async move {
            let mut authorized = node.load_authorized_peers()?;
            let mut swarm = node.take_swarm()?;
            allow_authorized_peers(&mut swarm, &authorized)?;
            node.run_loop(&mut swarm, &mut authorized, &mut commands_rx)
                .await
        });
        (commands_tx, task)
    }

    async fn ask_distribute_keys(
        commands: &CommandSender,
        peers: Vec<PeerId>,
    ) -> Result<(), NodeError> {
        let (reply, answer) = oneshot::channel();
        commands
            .send(RelayerCommand::DistributeKeys { peers, reply })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), answer)
            .await
            .expect("an idle relayer must answer within 2 seconds")
            .unwrap()
    }

    #[tokio::test]
    async fn test_idle_relayer_answers_a_command_at_once() {
        let (commands, _task) = spawn_idle_loop(idle_relayer().await);
        let peer = random_peer();

        match ask_distribute_keys(&commands, vec![peer]).await {
            Err(NodeError::PeerError(message)) => {
                assert_eq!(message, not_connected_message(&peer))
            }
            other => panic!("expected PeerError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_run_loop_survives_a_caller_that_left() {
        let (logs, _guard) = capture_logs();
        let (commands, _task) = spawn_idle_loop(idle_relayer().await);
        let left = "The RPC caller left before the relayer answered";

        // A caller that waits for its answer is no news.
        assert!(ask_distribute_keys(&commands, vec![random_peer()])
            .await
            .is_err());
        assert!(!logged(&logs, left));

        let (reply, answer) = oneshot::channel();
        drop(answer);
        commands
            .send(RelayerCommand::DistributeKeys {
                peers: vec![random_peer()],
                reply,
            })
            .await
            .unwrap();

        assert!(ask_distribute_keys(&commands, vec![random_peer()])
            .await
            .is_err());
        assert!(logged(&logs, left));
    }

    #[tokio::test]
    async fn test_run_loop_ends_when_the_command_channel_closes() {
        let (commands, task) = spawn_idle_loop(idle_relayer().await);
        drop(commands);

        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("the loop must end when the RPC server is gone")
            .unwrap();
        match result {
            Err(NodeError::TaskError(message)) => {
                assert_eq!(message, "The RPC command channel closed")
            }
            other => panic!("expected TaskError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_run_loop_stops_when_the_relayer_disconnects() {
        let mut node = idle_relayer().await;
        node.base.state = NodeState::Disconnected;
        let (_commands, task) = spawn_idle_loop(node);

        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a disconnected relayer must not loop")
            .unwrap();
        assert!(result.is_ok());
    }

    // Lets `peer` connect, as an authorized peer.
    fn admit(relayer: &mut Swarm<NodeBehaviour>, peer: PeerId) {
        allow_list(relayer).unwrap().allow_peer(peer);
    }

    // A relayer listening on loopback and subscribed to its topic, as `run`
    // sets it up, with the swarm in the test's hands.
    async fn listening_relayer() -> (RelayerNode, Swarm<NodeBehaviour>, Multiaddr) {
        let mut node = idle_relayer().await;
        node.init_gossipsub_topic().await.unwrap();
        let mut swarm = node.take_swarm().unwrap();
        let address = listen(&mut swarm).await;
        (node, swarm, address)
    }

    // Runs the relayer's handler on every relayer event and hands every peer
    // event, and a tick every 100 ms, to `step` until it returns true.
    async fn drive<F>(
        node: &mut RelayerNode,
        relayer: &mut Swarm<NodeBehaviour>,
        peer: &mut Swarm<NodeBehaviour>,
        mut step: F,
    ) where
        F: FnMut(
            &mut RelayerNode,
            &mut Swarm<NodeBehaviour>,
            &mut Swarm<NodeBehaviour>,
            Option<SwarmEvent<NodeBehaviourEvent>>,
        ) -> bool,
    {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let done = tokio::select! {
                    event = relayer.select_next_some() => {
                        node.handle_relayer_events(relayer, event).await;
                        step(node, relayer, peer, None)
                    }
                    event = peer.select_next_some() => step(node, relayer, peer, Some(event)),
                    _ = tick.tick() => step(node, relayer, peer, None),
                };
                if done {
                    return;
                }
            }
        })
        .await
        .expect("the relayer did not handle the peer in time");
    }

    #[tokio::test]
    async fn test_relayer_answers_a_key_request() {
        let (mut node, mut relayer, address) = listening_relayer().await;
        let relayer_id = *relayer.local_peer_id();
        let mut peer = bare_swarm().await;
        admit(&mut relayer, *peer.local_peer_id());
        peer.dial(address).unwrap();

        let mut answer = None;
        drive(&mut node, &mut relayer, &mut peer, |_, _, peer, event| {
            match event {
                Some(SwarmEvent::ConnectionEstablished { peer_id, .. })
                    if peer_id == relayer_id =>
                {
                    peer.behaviour_mut()
                        .key_exchange
                        .send_request(&relayer_id, PartialKeyRequest { node_index: 7 });
                }
                Some(SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(
                    request_response::Event::Message {
                        message: request_response::Message::Response { response, .. },
                        ..
                    },
                ))) => answer = Some(response),
                Some(SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(
                    request_response::Event::OutboundFailure { error, .. },
                ))) => panic!("the relayer did not answer: {:?}", error),
                _ => {}
            }
            answer.is_some()
        })
        .await;

        let answer = answer.unwrap();
        assert_eq!(answer.node_index, 7);
        assert!(answer.key_data.is_empty());
    }

    #[tokio::test]
    async fn test_relayer_routes_a_peer_from_its_connection() {
        let (mut node, mut relayer, address) = listening_relayer().await;
        // The peer listens nowhere, so identify gives the relayer no address:
        // the only one it can learn is the connection's.
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        admit(&mut relayer, *peer.local_peer_id());
        peer.dial(address).unwrap();

        drive(&mut node, &mut relayer, &mut peer, |node, relayer, _, _| {
            !kademlia_addresses(relayer, &peer_id).is_empty()
                && node.base.config.is_bootstrap_started
        })
        .await;
    }

    #[tokio::test]
    async fn test_relayer_routes_the_addresses_a_peer_identifies() {
        let (mut node, mut relayer, address) = listening_relayer().await;
        // Two listen addresses: a connection carries one at most, identify
        // carries both.
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        let first = listen(&mut peer).await;
        let second = listen(&mut peer).await;
        admit(&mut relayer, *peer.local_peer_id());
        peer.dial(address).unwrap();

        drive(&mut node, &mut relayer, &mut peer, |_, relayer, _, _| {
            let known = kademlia_addresses(relayer, &peer_id);
            known.contains(&first) && known.contains(&second)
        })
        .await;
    }

    #[tokio::test]
    async fn test_relayer_counts_each_gossipsub_message() {
        let (mut node, mut relayer, address) = listening_relayer().await;
        let mut peer = bare_swarm().await;
        admit(&mut relayer, *peer.local_peer_id());
        peer.dial(address).unwrap();
        let topic = gossipsub::IdentTopic::new("custom_events");

        let mut published = false;
        drive(&mut node, &mut relayer, &mut peer, |node, _, peer, _| {
            // Publishing fails until the peer learns the relayer's topic.
            if !published {
                published = peer
                    .behaviour_mut()
                    .gossipsub
                    .publish(topic.clone(), b"hello".to_vec())
                    .is_ok();
            }
            node.base.metrics.messages_received > 0
        })
        .await;

        assert_eq!(node.base.metrics.messages_received, 1);
    }

    #[tokio::test]
    async fn test_distribute_keys_sends_each_connected_peer_its_index() {
        let (mut node, mut relayer, address) = listening_relayer().await;
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        admit(&mut relayer, *peer.local_peer_id());
        peer.dial(address).unwrap();

        let mut sent = false;
        let mut received = None;
        drive(
            &mut node,
            &mut relayer,
            &mut peer,
            |_, relayer, _, event| {
                if !sent && relayer.is_connected(&peer_id) {
                    // The peer twice: indexes are positions in the list.
                    distribute_keys(relayer, &[peer_id, peer_id]).unwrap();
                    sent = true;
                }
                if let Some(SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(
                    request_response::Event::Message {
                        message: request_response::Message::Request { request, .. },
                        ..
                    },
                ))) = event
                {
                    received
                        .get_or_insert_with(Vec::new)
                        .push(request.node_index);
                }
                received.as_ref().is_some_and(|indexes| indexes.len() == 2)
            },
        )
        .await;

        let mut indexes = received.unwrap();
        indexes.sort();
        assert_eq!(indexes, vec![0, 1]);
    }

    #[tokio::test]
    async fn test_relayer_logs_identify_and_kademlia_progress() {
        let (logs, _guard) = capture_logs();
        let (mut node, mut relayer, address) = listening_relayer().await;
        let mut peer = bare_swarm().await;
        admit(&mut relayer, *peer.local_peer_id());
        peer.dial(address).unwrap();

        // The routing update starts a closest-peers query, and the first
        // routed peer an automatic bootstrap.
        drive(&mut node, &mut relayer, &mut peer, |_, _, _, _| {
            [
                "Sent identify info to ",
                "Found closest peers: ",
                "Bootstrap succeeded with stats: ",
            ]
            .iter()
            .all(|prefix| logged(&logs, prefix))
        })
        .await;
    }

    #[tokio::test]
    async fn test_relayer_logs_its_listen_address() {
        let (logs, _guard) = capture_logs();
        let mut node = idle_relayer().await;
        node.base.init_network_listener().await.unwrap();
        let mut swarm = node.take_swarm().unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            while !logged(&logs, "Listening on /ip4/127.0.0.1/udp/") {
                let event = swarm.select_next_some().await;
                node.handle_relayer_events(&mut swarm, event).await;
            }
        })
        .await
        .expect("the relayer did not log its listen address");
    }

    fn allowed(swarm: &mut Swarm<NodeBehaviour>) -> Vec<PeerId> {
        let mut peers: Vec<PeerId> = allow_list(swarm)
            .unwrap()
            .allowed_peers()
            .iter()
            .copied()
            .collect();
        peers.sort();
        peers
    }

    fn in_memory_authorized() -> AuthorizedPeers {
        AuthorizedPeers::load(generate_keypair(None), None).unwrap()
    }

    #[tokio::test]
    async fn test_a_relayer_allows_no_peer_until_it_loads_its_list() {
        let mut swarm = idle_relayer().await.take_swarm().unwrap();
        assert!(allowed(&mut swarm).is_empty());

        let mut authorized = in_memory_authorized();
        let (a, b) = (random_peer(), random_peer());
        authorized.authorize(&[a, b]).unwrap();
        allow_authorized_peers(&mut swarm, &authorized).unwrap();

        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(allowed(&mut swarm), expected);
    }

    #[tokio::test]
    async fn test_a_relayer_without_its_allow_list_does_not_run() {
        // Built as a validator's: the allow list is disabled.
        let mut swarm = bare_swarm().await;
        assert!(allow_list(&mut swarm).is_none());

        assert!(matches!(
            allow_authorized_peers(&mut swarm, &in_memory_authorized()),
            Err(NodeError::ConfigError(_))
        ));
    }

    #[tokio::test]
    async fn test_authorize_and_revoke_nodes_follow_the_allow_list() {
        let mut swarm = idle_relayer().await.take_swarm().unwrap();
        let mut authorized = in_memory_authorized();
        let (a, b) = (random_peer(), random_peer());

        let state = authorize_nodes(&mut swarm, &mut authorized, &[a, b]).unwrap();
        assert_eq!(state, authorized.state());
        assert_eq!(state.peer_ids.len(), 2);
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(allowed(&mut swarm), expected);

        let state = revoke_nodes(&mut swarm, &mut authorized, &[a]).unwrap();
        assert_eq!(state.peer_ids, vec![b.to_string()]);
        assert_eq!(allowed(&mut swarm), vec![b]);

        assert!(authorize_nodes(&mut swarm, &mut authorized, &[]).is_err());
        assert!(revoke_nodes(&mut swarm, &mut authorized, &[]).is_err());
        assert_eq!(allowed(&mut swarm), vec![b]);
    }

    #[tokio::test]
    async fn test_the_run_loop_serves_the_authorization_commands() {
        let (commands, _task) = spawn_idle_loop(idle_relayer().await);
        let peer = random_peer();
        async fn ask<T>(
            commands: &CommandSender,
            make: impl FnOnce(oneshot::Sender<T>) -> RelayerCommand,
        ) -> T {
            let (reply, answer) = oneshot::channel();
            commands.send(make(reply)).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), answer)
                .await
                .unwrap()
                .unwrap()
        }

        let day_one = ask(&commands, |reply| RelayerCommand::AuthorizedNodes { reply }).await;
        assert!(day_one.peer_ids.is_empty());

        let added = ask(&commands, |reply| RelayerCommand::AuthorizeNodes {
            peers: vec![peer],
            reply,
        })
        .await
        .unwrap();
        assert_eq!(added.peer_ids, vec![peer.to_string()]);
        assert!(added.timestamp > day_one.timestamp);
        assert_eq!(
            ask(&commands, |reply| RelayerCommand::AuthorizedNodes { reply }).await,
            added
        );

        let revoked = ask(&commands, |reply| RelayerCommand::RevokeNodes {
            peers: vec![peer],
            reply,
        })
        .await
        .unwrap();
        assert!(revoked.peer_ids.is_empty());
        assert!(revoked.timestamp > added.timestamp);

        let refused = ask(&commands, |reply| RelayerCommand::AuthorizeNodes {
            peers: vec![],
            reply,
        })
        .await;
        assert!(matches!(refused, Err(NodeError::AuthorizedPeers(_))));
    }

    #[tokio::test]
    async fn test_relayer_denies_an_unauthorized_peer_and_logs_it() {
        let (logs, _guard) = capture_logs();
        let (mut node, mut relayer, address) = listening_relayer().await;
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        peer.dial(address).unwrap();
        let denial = format!("Denied a connection from unauthorized peer {}", peer_id);

        drive(&mut node, &mut relayer, &mut peer, |_, _, _, _| {
            logged(&logs, &denial)
        })
        .await;

        assert!(!relayer.is_connected(&peer_id));
        // No behaviour saw the connection: Kademlia learned nothing of it.
        assert!(kademlia_addresses(&mut relayer, &peer_id).is_empty());
        assert!(!logged(
            &logs,
            &format!("Connected established to {}", peer_id)
        ));
    }

    #[tokio::test]
    async fn test_an_authorized_peer_connects_and_a_revoked_one_is_cut_off() {
        let (logs, _guard) = capture_logs();
        let (mut node, mut relayer, address) = listening_relayer().await;
        let mut authorized = in_memory_authorized();
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        authorize_nodes(&mut relayer, &mut authorized, &[peer_id]).unwrap();
        peer.dial(address).unwrap();

        drive(&mut node, &mut relayer, &mut peer, |_, relayer, _, _| {
            relayer.is_connected(&peer_id)
        })
        .await;

        revoke_nodes(&mut relayer, &mut authorized, &[peer_id]).unwrap();
        let closed = format!("Connection closed with {}", peer_id);
        drive(&mut node, &mut relayer, &mut peer, |_, relayer, _, _| {
            !relayer.is_connected(&peer_id) && logged(&logs, &closed)
        })
        .await;
    }

    #[tokio::test]
    async fn test_only_a_denial_by_the_allow_list_is_logged_as_unauthorized() {
        let (logs, _guard) = capture_logs();
        let mut node = idle_relayer().await;
        let mut swarm = node.take_swarm().unwrap();
        let peer = random_peer();
        // A denial for another cause than the allow list's.
        let event = SwarmEvent::IncomingConnectionError {
            connection_id: libp2p::swarm::ConnectionId::new_unchecked(0),
            local_addr: "/ip4/127.0.0.1/udp/1/quic-v1".parse().unwrap(),
            send_back_addr: "/ip4/127.0.0.1/udp/2/quic-v1".parse().unwrap(),
            error: ListenError::Denied {
                cause: libp2p::swarm::ConnectionDenied::new(std::io::Error::other("limit")),
            },
            peer_id: Some(peer),
        };

        node.handle_relayer_events(&mut swarm, event).await;

        assert!(!logged(&logs, "Denied a connection from unauthorized peer"));
    }

    #[tokio::test]
    async fn test_a_relayer_that_cannot_listen_stops_with_an_error() {
        let mut config = test_config(true);
        config.listen_address = "not-an-ip".to_string();
        let mut node = RelayerNode::new(config);

        let result = tokio::time::timeout(Duration::from_secs(5), node.run())
            .await
            .expect("a relayer that cannot listen must stop at once");

        assert!(
            matches!(result, Err(NodeError::MultiAddrError(_))),
            "{:?}",
            result
        );
    }
}
