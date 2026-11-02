use futures::StreamExt;
use libp2p::{gossipsub, request_response, PeerId, Swarm};
use libp2p::{identity::Keypair, swarm::SwarmEvent};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use super::common::{
    bootstrap_peer_id, BaseNode, NodeCommon, AUTHORIZED_PEERS_TOPIC, CUSTOM_EVENTS_TOPIC,
};
use super::held::{Accepted, HeldTree, Refusal};
use crate::utils::auth::{AuthorizedPeersRequest, WireTree};
use crate::{
    types::{NodeBehaviourEvent, NodeConfig, NodeError},
    CustomEvent, NodeBehaviour, NodeMetrics, NodeState,
};
use crate::{PartialKeyRequest, PartialKeyResponse};

/// Lets the peers the held tree allows connect, and closes the connections
/// of those it no longer allows. Fails when the swarm has no allow list: the
/// validator would accept any peer.
fn allow_held_peers(swarm: &mut Swarm<NodeBehaviour>, held: &HeldTree) -> Result<(), NodeError> {
    let allow_list = swarm.behaviour_mut().allow_list.as_mut().ok_or_else(|| {
        NodeError::ConfigError("The validator's allow list is disabled".to_string())
    })?;
    for peer in held.allowed() {
        allow_list.allow_peer(peer);
    }
    Ok(())
}

/// How often a validator asks its relayer for the current tree, besides on
/// each connection to it. A push can be lost: gossipsub drops a message for a
/// peer whose send queue is full, which an authorized peer can provoke by
/// flooding. The pull, which does not go through that queue, bounds how long
/// a lost push leaves a revoked peer allowed.
const TREE_PULL_PERIOD: Duration = Duration::from_secs(15);

/// Sends the relayer a request for its current tree.
fn request_tree(swarm: &mut Swarm<NodeBehaviour>, relayer: &PeerId) {
    swarm
        .behaviour_mut()
        .authorized_peers
        .send_request(relayer, AuthorizedPeersRequest {});
}

/// Asks the relayer for its current tree, when the validator connects to it.
fn ask_for_tree(swarm: &mut Swarm<NodeBehaviour>, relayer: &PeerId) {
    request_tree(swarm, relayer);
    info!(
        "Asked the relayer {} for the authorized peers tree",
        relayer
    );
}

/// Applies what accepting a tree changed: the added peers may connect, the
/// removed ones are cut off. When the tree allows new peers, discovery runs
/// again: the dials it made to them before were denied, and nothing retries
/// them before Kademlia's periodic bootstrap.
fn enforce(swarm: &mut Swarm<NodeBehaviour>, outcome: Result<Accepted, Refusal>, from: &PeerId) {
    let accepted = match outcome {
        Ok(accepted) => accepted,
        Err(Refusal::AlreadyHeld) => {
            debug!("The authorized peers tree from {} is the one held", from);
            return;
        }
        Err(refusal) => {
            warn!(
                "Refused the authorized peers tree from {}: {}",
                from, refusal
            );
            return;
        }
    };
    info!(
        "Accepted the authorized peers tree from {}: {} peers, root {}, timestamp {}",
        from,
        accepted.peers,
        hex::encode(accepted.root),
        accepted.timestamp
    );
    let behaviour = swarm.behaviour_mut();
    if let Some(allow_list) = behaviour.allow_list.as_mut() {
        for peer in &accepted.added {
            allow_list.allow_peer(*peer);
        }
        for peer in &accepted.removed {
            // Closes the peer's connections, too.
            allow_list.disallow_peer(*peer);
        }
    }
    for peer in &accepted.removed {
        behaviour.gossipsub.remove_explicit_peer(peer);
        info!("Disallowed peer {}: its connections are closed", peer);
    }
    if !accepted.added.is_empty() {
        match behaviour.kademlia.bootstrap() {
            Ok(_) => info!(
                "Discovering peers again: the tree allows {} new peers",
                accepted.added.len()
            ),
            Err(e) => warn!("Cannot discover the newly allowed peers: {:?}", e),
        }
    }
}

#[derive(Clone)]
pub struct ValidatorNode {
    base: BaseNode,
    held: HeldTree,
    /// How often the run loop asks the relayer for its tree.
    pull_period: Duration,
}

impl NodeCommon for ValidatorNode {
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

impl ValidatorNode {
    /// A validator of the relayer `config.bootstrap_peer_id`, whose ID must
    /// embed the key that signs the trees.
    pub fn new(config: NodeConfig) -> Result<Self, NodeError> {
        let held = HeldTree::new(bootstrap_peer_id(&config)?)?;
        Ok(Self {
            base: BaseNode::new(config),
            held,
            pull_period: TREE_PULL_PERIOD,
        })
    }

    async fn init_gossipsub_topic(&mut self) -> Result<gossipsub::IdentTopic, NodeError> {
        let topic = gossipsub::IdentTopic::new(CUSTOM_EVENTS_TOPIC);
        let trees = gossipsub::IdentTopic::new(AUTHORIZED_PEERS_TOPIC);

        let swarm = self
            .base
            .swarm
            .as_ref()
            .ok_or(NodeError::SwarmNotInitialized)?;
        let mut swarm = swarm.lock().await;

        for topic in [&topic, &trees] {
            swarm
                .behaviour_mut()
                .gossipsub
                .subscribe(topic)
                .map_err(|e| NodeError::ConfigError(e.to_string()))?;
            info!("Subscribed to topic: {}", topic);
        }

        Ok(topic)
    }

    // Before the node listens or dials: until it holds a tree, it allows the
    // relayer only.
    async fn init_allow_list(&mut self) -> Result<(), NodeError> {
        let swarm = self
            .base
            .swarm
            .as_ref()
            .ok_or(NodeError::SwarmNotInitialized)?;
        allow_held_peers(&mut *swarm.lock().await, &self.held)
    }

    async fn handle_authorized_peers_event(
        &mut self,
        event: request_response::Event<AuthorizedPeersRequest, WireTree>,
    ) {
        match event {
            request_response::Event::Message {
                peer,
                message: request_response::Message::Response { response, .. },
                ..
            } => {
                let outcome = self.held.accept_wire(response);
                if let Some(swarm) = &self.base.swarm {
                    enforce(&mut *swarm.lock().await, outcome, &peer);
                }
            }
            // Only the relayer serves the tree: dropping the channel answers
            // the request with a failure.
            request_response::Event::Message { peer, .. } => {
                debug!(
                    "Ignored a request for the authorized peers tree from {}",
                    peer
                );
            }
            request_response::Event::OutboundFailure { peer, error, .. } => {
                warn!(
                    "Failed to get the authorized peers tree from {}: {}",
                    peer, error
                );
            }
            other => debug!("Authorized peers event: {:?}", other),
        }
    }

    async fn handle_key_exchange_event(
        &mut self,
        event: request_response::Event<PartialKeyRequest, PartialKeyResponse>,
    ) {
        match event {
            request_response::Event::Message { peer, message, .. } => {
                match message {
                    request_response::Message::Request {
                        request_id,
                        request,
                        channel,
                    } => {
                        info!(
                            "Received key distribution request from {} with id {} for index {}",
                            peer, request_id, request.node_index
                        );

                        // Here you would implement your actual key generation/distribution logic
                        let key_data = self.generate_partial_key(request.node_index).await;

                        if let Some(swarm) = &self.base.swarm {
                            let mut swarm = swarm.lock().await;
                            let response = PartialKeyResponse {
                                key_data,
                                node_index: request.node_index,
                            };

                            info!(
                                "Sending key response to relayer for index {}",
                                request.node_index
                            );
                            if let Err(e) = swarm
                                .behaviour_mut()
                                .key_exchange
                                .send_response(channel, response)
                            {
                                error!("Failed to send key response: {:?}", e);
                            }
                        }
                    }
                    _ => error!("Unexpected message type from {}", peer),
                }
            }
            request_response::Event::OutboundFailure { peer, error, .. } => {
                error!("Failed to send response to relayer {}: {:?}", peer, error);
            }
            request_response::Event::InboundFailure { peer, error, .. } => {
                error!(
                    "Failed to receive request from relayer {}: {:?}",
                    peer, error
                );
            }
            request_response::Event::ResponseSent {
                peer, request_id, ..
            } => {
                info!(
                    "Successfully sent response to relayer {} for request {}",
                    peer, request_id
                );
            }
        }
    }

    async fn generate_partial_key(&self, index: usize) -> Vec<u8> {
        // Implement your actual key generation logic here
        // This is just a placeholder
        vec![index as u8; 32]
    }

    async fn publish_blockchain_event(&mut self) -> Result<(), NodeError> {
        if !self.base.config.gen_msg {
            return Ok(());
        }

        let swarm = self
            .base
            .swarm
            .as_ref()
            .ok_or_else(|| NodeError::SwarmNotInitialized)?;
        let swarm = swarm.clone();

        let topic = gossipsub::IdentTopic::new(CUSTOM_EVENTS_TOPIC);
        let topic = topic.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                let mut swarm_lock = swarm.lock().await;
                let behaviour = swarm_lock.behaviour_mut();
                let event = CustomEvent::new().unwrap();
                let message = serde_json::to_string(&event).unwrap();

                if let Err(e) = behaviour
                    .gossipsub
                    .publish(topic.clone(), message.as_bytes())
                {
                    error!("Failed to publish event: {:?}", e);
                } else {
                    info!("Event published successfully");
                }
            }
        });

        Ok(())
    }

    // async fn store_partial_key(&mut self, response: PartialKeyResponse) {
    //     // Store the partial key securely
    //     info!("Storing partial key {}", response.node_index);
    // }

    async fn handle_validator_events(&mut self, event: SwarmEvent<NodeBehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(NodeBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) if message.topic == gossipsub::IdentTopic::new(AUTHORIZED_PEERS_TOPIC).hash() => {
                let outcome = self.held.accept_bytes(&message.data);
                if let Some(swarm) = &self.base.swarm {
                    enforce(&mut *swarm.lock().await, outcome, &propagation_source);
                }
            }

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
                self.handle_key_exchange_event(event).await;
            }

            SwarmEvent::Behaviour(NodeBehaviourEvent::AuthorizedPeers(event)) => {
                self.handle_authorized_peers_event(event).await;
            }

            _ => {
                // Handle common events through the base implementation
                if let Some(swarm) = self.base.swarm.clone() {
                    let mut swarm = swarm.lock().await;
                    // Each connection to the relayer asks for its tree, so a
                    // validator that missed a publication catches up.
                    if let SwarmEvent::ConnectionEstablished { peer_id, .. } = &event {
                        if *peer_id == self.held.relayer() {
                            ask_for_tree(&mut swarm, peer_id);
                        }
                    }
                    self.base.handle_common_events(&mut swarm, event).await;
                }
            }
        }
    }

    // Everything `run` sets up before its loop.
    async fn start(&mut self) -> Result<(), NodeError> {
        self.base.init_swarm().await?;
        self.init_allow_list().await?;
        self.base.init_network_listener().await?;
        self.base.connect_to_bootstrap().await?;
        self.base.state = NodeState::Connected;
        self.init_gossipsub_topic().await?;
        self.publish_blockchain_event().await
    }

    /// The timer of the periodic pulls. Its first tick comes one period after
    /// the start: the connection to the relayer asks for the tree first.
    fn pull_timer(&self) -> Interval {
        let mut timer =
            tokio::time::interval_at(Instant::now() + self.pull_period, self.pull_period);
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        timer
    }

    /// Handles the next swarm event or, when the pull timer ticks first, asks
    /// the relayer for its tree. A relayer that is not connected is dialed.
    async fn step(&mut self, swarm: &Arc<Mutex<Swarm<NodeBehaviour>>>, pull: &mut Interval) {
        let event = {
            let mut swarm = swarm.lock().await;
            tokio::select! {
                event = swarm.next() => event,
                _ = pull.tick() => {
                    request_tree(&mut swarm, &self.held.relayer());
                    debug!(
                        "Asked the relayer {} for the authorized peers tree, as every {:?}",
                        self.held.relayer(),
                        self.pull_period
                    );
                    None
                }
            }
        };

        if let Some(event) = event {
            self.handle_validator_events(event).await;
        }
    }

    pub async fn run(&mut self) -> Result<(), NodeError> {
        info!("🔶 Validator node");
        self.start().await?;

        let swarm = Arc::clone(
            self.base
                .swarm
                .as_ref()
                .expect("Swarm should be initialized"),
        );

        let mut pull = self.pull_timer();
        while self.base.state != NodeState::Disconnected {
            self.step(&swarm, &mut pull).await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::common::test_support::{
        bare_swarm, bare_swarm_of, capture_logs, kademlia_addresses, listen, logged, test_config,
    };
    use crate::utils::auth::AuthorizedPeersTree;
    use crate::utils::peer_id::generate_keypair;
    use futures::StreamExt;
    use libp2p::Multiaddr;
    use std::collections::BTreeSet;

    fn random_peer() -> PeerId {
        generate_keypair(None).public().to_peer_id()
    }

    fn config_for(relayer: &PeerId) -> NodeConfig {
        let mut config = test_config(false);
        config.bootstrap_peer_id = Some(relayer.to_string());
        config
    }

    // A validator of `relayer`, with its allow list and its topics, as `run`
    // sets it up, and its swarm shared as `run` shares it.
    async fn validator_of(relayer: &PeerId) -> ValidatorNode {
        let mut node = ValidatorNode::new(config_for(relayer)).unwrap();
        node.base.init_swarm().await.unwrap();
        node.init_allow_list().await.unwrap();
        node.init_gossipsub_topic().await.unwrap();
        node
    }

    // A validator of `relayer`, dialing it at `address`.
    async fn validator_dialing(relayer: &PeerId, address: Multiaddr) -> ValidatorNode {
        let node = validator_of(relayer).await;
        node.base
            .swarm
            .as_ref()
            .unwrap()
            .lock()
            .await
            .dial(address)
            .unwrap();
        node
    }

    fn shared(node: &ValidatorNode) -> Arc<Mutex<Swarm<NodeBehaviour>>> {
        Arc::clone(node.base.swarm.as_ref().unwrap())
    }

    fn allowed(swarm: &mut Swarm<NodeBehaviour>) -> BTreeSet<PeerId> {
        swarm
            .behaviour_mut()
            .allow_list
            .as_mut()
            .unwrap()
            .allowed_peers()
            .iter()
            .copied()
            .collect()
    }

    // A relayer signing trees with strictly increasing timestamps.
    struct Signer {
        keypair: Keypair,
        tree: AuthorizedPeersTree,
    }

    impl Signer {
        fn new() -> Self {
            Self {
                keypair: generate_keypair(None),
                tree: AuthorizedPeersTree::new(),
            }
        }

        fn id(&self) -> PeerId {
            self.keypair.public().to_peer_id()
        }

        fn sign(&mut self, peers: &[PeerId]) -> AuthorizedPeersTree {
            self.tree
                .update_tree(peers.to_vec(), &self.keypair)
                .unwrap();
            self.tree.clone()
        }
    }

    fn wire_bytes(tree: &AuthorizedPeersTree) -> Vec<u8> {
        WireTree::from(tree).encode().unwrap()
    }

    // Runs the validator's handler on every validator event, never holding
    // the swarm lock while it does, and hands every event of the other
    // peers, with the peer's index, and a tick every 100 ms, to `step` until
    // it returns true.
    async fn drive<F>(node: &mut ValidatorNode, peers: &mut [Swarm<NodeBehaviour>], mut step: F)
    where
        F: FnMut(
            &mut ValidatorNode,
            &mut [Swarm<NodeBehaviour>],
            Option<(usize, SwarmEvent<NodeBehaviourEvent>)>,
        ) -> bool,
    {
        let swarm = shared(node);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (validator_event, peer_event) = {
                    let mut validator = swarm.lock().await;
                    let peer_events = futures::future::select_all(
                        peers.iter_mut().map(|peer| peer.select_next_some()),
                    );
                    tokio::select! {
                        event = validator.select_next_some() => (Some(event), None),
                        (event, index, _) = peer_events => (None, Some((index, event))),
                        _ = tick.tick() => (None, None),
                    }
                };
                if let Some(event) = validator_event {
                    node.handle_validator_events(event).await;
                }
                if step(node, peers, peer_event) {
                    return;
                }
            }
        })
        .await
        .expect("the validator did not handle the peers in time");
    }

    #[tokio::test]
    async fn test_validator_answers_a_key_request_with_its_partial_key() {
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        let address = listen(&mut peer).await;
        let mut node = validator_dialing(&peer_id, address).await;
        let validator_id = node.base.keypair.as_ref().unwrap().public().to_peer_id();

        let mut answer = None;
        drive(&mut node, &mut [peer], |_, peers, event| {
            match event {
                Some((_, SwarmEvent::ConnectionEstablished { peer_id, .. }))
                    if peer_id == validator_id =>
                {
                    peers[0]
                        .behaviour_mut()
                        .key_exchange
                        .send_request(&validator_id, PartialKeyRequest { node_index: 3 });
                }
                Some((
                    _,
                    SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(
                        request_response::Event::Message {
                            message: request_response::Message::Response { response, .. },
                            ..
                        },
                    )),
                )) => answer = Some(response),
                Some((
                    _,
                    SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(
                        request_response::Event::OutboundFailure { error, .. },
                    )),
                )) => panic!("the validator did not answer: {:?}", error),
                _ => {}
            }
            answer.is_some()
        })
        .await;

        let answer = answer.unwrap();
        assert_eq!(answer.node_index, 3);
        assert_eq!(answer.key_data, vec![3u8; 32]);
    }

    #[tokio::test]
    async fn test_validator_counts_each_gossipsub_message() {
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        let address = listen(&mut peer).await;
        let mut node = validator_dialing(&peer_id, address).await;
        let topic = gossipsub::IdentTopic::new("custom_events");

        let mut published = false;
        drive(&mut node, &mut [peer], |node, peers, _| {
            // Publishing fails until the peer learns the validator's topic.
            if !published {
                published = peers[0]
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
    async fn test_validator_routes_the_peer_it_connected_to() {
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        let address = listen(&mut peer).await;
        let mut node = validator_dialing(&peer_id, address).await;

        drive(&mut node, &mut [peer], |node, _, _| {
            let swarm = shared(node);
            let mut swarm = swarm.try_lock().unwrap();
            !kademlia_addresses(&mut swarm, &peer_id).is_empty()
        })
        .await;
    }

    #[tokio::test]
    async fn test_a_validator_that_cannot_listen_stops_with_an_error() {
        let mut config = config_for(&random_peer());
        config.listen_address = "not-an-ip".to_string();
        let mut node = ValidatorNode::new(config).unwrap();

        let result = tokio::time::timeout(Duration::from_secs(5), node.run())
            .await
            .expect("a validator that cannot listen must stop at once");

        assert!(
            matches!(result, Err(NodeError::MultiAddrError(_))),
            "{:?}",
            result
        );
    }

    fn config_error(result: Result<ValidatorNode, NodeError>) -> String {
        match result {
            Err(NodeError::ConfigError(message)) => message,
            Err(other) => panic!("expected a ConfigError, got {:?}", other),
            Ok(_) => panic!("expected a ConfigError, got a validator"),
        }
    }

    #[test]
    fn test_a_bootstrap_peer_id_that_embeds_no_key_stops_the_validator() {
        let mut config = test_config(false);
        assert_eq!(
            config_error(ValidatorNode::new(config.clone())),
            "Missing bootstrap peer ID"
        );

        config.bootstrap_peer_id = Some("not-a-peer-id".to_string());
        assert!(config_error(ValidatorNode::new(config.clone()))
            .starts_with("Invalid bootstrap peer ID not-a-peer-id: "));

        // An identity multihash of 32 random bytes: no key.
        let keyless = PeerId::random();
        config.bootstrap_peer_id = Some(keyless.to_string());
        assert!(
            config_error(ValidatorNode::new(config)).starts_with(&format!(
                "The bootstrap peer ID {} embeds no public key",
                keyless
            ))
        );

        let relayer = random_peer();
        let node = ValidatorNode::new(config_for(&relayer)).unwrap();
        assert_eq!(node.held.relayer(), relayer);
    }

    #[tokio::test]
    async fn test_a_validator_allows_the_relayer_only_until_it_holds_a_tree() {
        let relayer = random_peer();
        let node = validator_of(&relayer).await;

        assert_eq!(
            allowed(&mut *shared(&node).lock().await),
            BTreeSet::from([relayer])
        );
    }

    #[tokio::test]
    async fn test_a_started_validator_allows_its_relayer_and_listens_to_the_trees() {
        let relayer = random_peer();
        let mut config = config_for(&relayer);
        config.bootstrap_address = Some("127.0.0.1".to_string());
        config.bootstrap_port = 1;
        let mut node = ValidatorNode::new(config).unwrap();

        node.start().await.unwrap();

        let swarm = shared(&node);
        let mut swarm = swarm.lock().await;
        assert_eq!(allowed(&mut swarm), BTreeSet::from([relayer]));
        let trees = gossipsub::IdentTopic::new("authorized_peers").hash();
        assert!(swarm.behaviour().gossipsub.topics().any(|t| *t == trees));
        assert_eq!(node.base.state, NodeState::Connected);
    }

    #[tokio::test]
    async fn test_gen_msg_needs_the_swarm_it_publishes_on() {
        let relayer = random_peer();
        let mut config = config_for(&relayer);
        config.gen_msg = true;
        let mut node = ValidatorNode::new(config).unwrap();

        assert!(matches!(
            node.publish_blockchain_event().await,
            Err(NodeError::SwarmNotInitialized)
        ));

        node.base.init_swarm().await.unwrap();
        assert!(node.publish_blockchain_event().await.is_ok());
    }

    #[tokio::test]
    async fn test_without_gen_msg_a_validator_publishes_nothing_and_needs_no_swarm() {
        let mut node = ValidatorNode::new(config_for(&random_peer())).unwrap();

        assert!(node.publish_blockchain_event().await.is_ok());
    }

    #[tokio::test]
    async fn test_a_validator_without_its_allow_list_does_not_run() {
        let held = HeldTree::new(random_peer()).unwrap();
        let mut swarm = bare_swarm().await;

        assert!(matches!(
            allow_held_peers(&mut swarm, &held),
            Err(NodeError::ConfigError(_))
        ));
    }

    #[tokio::test]
    async fn test_a_validator_subscribes_to_the_events_and_the_trees() {
        let node = validator_of(&random_peer()).await;
        let swarm = shared(&node);
        let swarm = swarm.lock().await;

        let topics: BTreeSet<String> = swarm
            .behaviour()
            .gossipsub
            .topics()
            .map(|topic| topic.to_string())
            .collect();
        assert_eq!(
            topics,
            BTreeSet::from(["custom_events".to_string(), "authorized_peers".to_string()])
        );
    }

    // A swarm with the allow list, the relayer allowed, as a validator's.
    fn enforcing_swarm(held: &HeldTree) -> Swarm<NodeBehaviour> {
        let mut swarm = BaseNode::build_swarm(&generate_keypair(None), true).unwrap();
        allow_held_peers(&mut swarm, held).unwrap();
        swarm
    }

    fn count(logs: &std::sync::Mutex<Vec<String>>, prefix: &str) -> usize {
        logs.lock()
            .unwrap()
            .iter()
            .filter(|message| message.starts_with(prefix))
            .count()
    }

    #[tokio::test]
    async fn test_enforce_follows_each_accepted_tree_and_rediscovers_new_peers() {
        let (logs, _guard) = capture_logs();
        let mut signer = Signer::new();
        let relayer = signer.id();
        let mut held = HeldTree::new(relayer).unwrap();
        let mut swarm = enforcing_swarm(&held);
        swarm
            .behaviour_mut()
            .kademlia
            .add_address(&relayer, "/ip4/127.0.0.1/udp/9/quic-v1".parse().unwrap());
        let (a, b) = (random_peer(), random_peer());
        let rediscovery = "Discovering peers again: the tree allows ";

        // A first tree.
        let tree = signer.sign(&[a, b]);
        enforce(&mut swarm, held.accept(tree.clone()), &relayer);
        assert_eq!(allowed(&mut swarm), BTreeSet::from([relayer, a, b]));
        assert!(logged(
            &logs,
            &format!(
                "Accepted the authorized peers tree from {}: 2 peers, root {}, timestamp {}",
                relayer,
                hex::encode(tree.root),
                tree.timestamp
            )
        ));
        assert!(logged(&logs, &format!("{}2 new peers", rediscovery)));

        // A revocation: no new peer, no discovery.
        enforce(&mut swarm, held.accept(signer.sign(&[a])), &relayer);
        assert_eq!(allowed(&mut swarm), BTreeSet::from([relayer, a]));
        assert!(logged(
            &logs,
            &format!("Disallowed peer {}: its connections are closed", b)
        ));
        assert_eq!(count(&logs, rediscovery), 1);

        // An addition.
        let c = random_peer();
        enforce(&mut swarm, held.accept(signer.sign(&[a, c])), &relayer);
        assert_eq!(allowed(&mut swarm), BTreeSet::from([relayer, a, c]));
        assert_eq!(count(&logs, rediscovery), 2);
        assert!(!logged(&logs, "Refused the authorized peers tree"));

        // A refusal changes nothing.
        let stale = tree;
        enforce(&mut swarm, held.accept(stale.clone()), &relayer);
        assert_eq!(allowed(&mut swarm), BTreeSet::from([relayer, a, c]));
        assert!(logged(
            &logs,
            &format!(
                "Refused the authorized peers tree from {}: its timestamp {} is not newer",
                relayer, stale.timestamp
            )
        ));
        assert_eq!(count(&logs, "Accepted the authorized peers tree"), 3);

        // The tree held, received again, is no refusal.
        enforce(&mut swarm, held.accept(signer.tree.clone()), &relayer);
        assert_eq!(allowed(&mut swarm), BTreeSet::from([relayer, a, c]));
        assert_eq!(count(&logs, "Refused the authorized peers tree"), 1);
        assert_eq!(count(&logs, "Accepted the authorized peers tree"), 3);
    }

    #[tokio::test]
    async fn test_a_rediscovery_without_known_peers_is_logged() {
        let (logs, _guard) = capture_logs();
        let mut signer = Signer::new();
        let mut held = HeldTree::new(signer.id()).unwrap();
        let mut swarm = enforcing_swarm(&held);

        enforce(
            &mut swarm,
            held.accept(signer.sign(&[random_peer()])),
            &signer.id(),
        );

        assert!(logged(&logs, "Cannot discover the newly allowed peers: "));
        assert!(!logged(&logs, "Discovering peers again"));
    }

    fn is_tree_request(event: &SwarmEvent<NodeBehaviourEvent>) -> bool {
        matches!(
            event,
            SwarmEvent::Behaviour(NodeBehaviourEvent::AuthorizedPeers(
                request_response::Event::Message {
                    message: request_response::Message::Request { .. },
                    ..
                }
            ))
        )
    }

    #[tokio::test]
    async fn test_a_validator_pulls_a_tree_of_100_peers_when_it_connects_to_the_relayer() {
        let (logs, _guard) = capture_logs();
        let mut signer = Signer::new();
        let peers: Vec<PeerId> = (0..100).map(|_| random_peer()).collect();
        let tree = signer.sign(&peers);
        let mut relayer = bare_swarm_of(&signer.keypair);
        let address = listen(&mut relayer).await;
        let mut node = validator_dialing(&signer.id(), address).await;

        let mut requests = 0;
        drive(&mut node, &mut [relayer], |node, peers, event| {
            if let Some((
                _,
                SwarmEvent::Behaviour(NodeBehaviourEvent::AuthorizedPeers(
                    request_response::Event::Message {
                        message: request_response::Message::Request { channel, .. },
                        ..
                    },
                )),
            )) = event
            {
                requests += 1;
                peers[0]
                    .behaviour_mut()
                    .authorized_peers
                    .send_response(channel, WireTree::from(&tree))
                    .unwrap();
            }
            node.held.tree().is_some()
        })
        .await;

        assert_eq!(requests, 1);
        let held = node.held.tree().unwrap();
        assert_eq!(held.peers.len(), 100);
        assert_eq!(held.root, tree.root);
        let mut expected: BTreeSet<PeerId> = peers.into_iter().collect();
        expected.insert(signer.id());
        assert_eq!(allowed(&mut *shared(&node).lock().await), expected);
        assert!(logged(&logs, "Asked the relayer "));
        assert!(logged(&logs, "Accepted the authorized peers tree from "));
    }

    // Answers each tree request with the tree `serving` holds when it comes,
    // and counts the requests, until the test ends.
    fn serve_trees(
        mut relayer: Swarm<NodeBehaviour>,
        serving: Arc<std::sync::Mutex<WireTree>>,
        requests: Arc<std::sync::atomic::AtomicUsize>,
    ) {
        tokio::spawn(async move {
            loop {
                if let SwarmEvent::Behaviour(NodeBehaviourEvent::AuthorizedPeers(
                    request_response::Event::Message {
                        message: request_response::Message::Request { channel, .. },
                        ..
                    },
                )) = relayer.select_next_some().await
                {
                    requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let tree = serving.lock().unwrap().clone();
                    let _ = relayer
                        .behaviour_mut()
                        .authorized_peers
                        .send_response(channel, tree);
                }
            }
        });
    }

    // Runs the validator's own loop step until `done` holds.
    async fn step_until<F>(node: &mut ValidatorNode, mut done: F)
    where
        F: FnMut(&ValidatorNode) -> bool,
    {
        let swarm = shared(node);
        let mut pull = node.pull_timer();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done(node) {
                node.step(&swarm, &mut pull).await;
                // A step that waits for nothing must not starve the timeout.
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the validator did not get there in time");
    }

    #[tokio::test]
    async fn test_a_periodic_pull_brings_a_revocation_whose_push_was_lost() {
        let (logs, _guard) = capture_logs();
        let mut signer = Signer::new();
        let revoked = random_peer();
        let first = signer.sign(&[revoked]);
        let mut relayer = bare_swarm_of(&signer.keypair);
        let address = listen(&mut relayer).await;
        let serving = Arc::new(std::sync::Mutex::new(WireTree::from(&first)));
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        serve_trees(relayer, Arc::clone(&serving), Arc::clone(&requests));
        let mut node = validator_dialing(&signer.id(), address).await;
        node.pull_period = Duration::from_millis(200);

        step_until(&mut node, |node| node.held.tree().is_some()).await;
        assert!(allowed(&mut *shared(&node).lock().await).contains(&revoked));

        // The relayer revokes the peer, and its push never arrives.
        let second = signer.sign(&[]);
        *serving.lock().unwrap() = WireTree::from(&second);
        step_until(&mut node, |node| {
            node.held.tree().unwrap().timestamp == second.timestamp
        })
        .await;

        assert!(!allowed(&mut *shared(&node).lock().await).contains(&revoked));
        // The next pulls bring the tree held back: nothing to refuse.
        let pulled = requests.load(std::sync::atomic::Ordering::SeqCst);
        assert!(pulled >= 2);
        step_until(&mut node, |_| {
            requests.load(std::sync::atomic::Ordering::SeqCst) > pulled + 1
        })
        .await;
        assert!(!logged(&logs, "Refused the authorized peers tree"));
        assert_eq!(node.held.tree().unwrap().timestamp, second.timestamp);
    }

    #[tokio::test]
    async fn test_the_first_periodic_pull_comes_one_period_after_the_start() {
        let mut node = ValidatorNode::new(config_for(&random_peer())).unwrap();
        node.pull_period = Duration::from_millis(300);
        let started = Instant::now();
        let mut pull = node.pull_timer();

        pull.tick().await;

        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[tokio::test]
    async fn test_a_pulled_tree_larger_than_a_push_fails_before_it_is_decoded() {
        let (logs, _guard) = capture_logs();
        let mut signer = Signer::new();
        let peers: Vec<PeerId> = (0..1300).map(|_| random_peer()).collect();
        let large = WireTree::from(&signer.sign(&peers));
        assert!(large.encode().unwrap().len() > crate::utils::auth::MAX_WIRE_TREE_BYTES);
        let mut relayer = bare_swarm_of(&signer.keypair);
        let address = listen(&mut relayer).await;
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        serve_trees(
            relayer,
            Arc::new(std::sync::Mutex::new(large)),
            Arc::clone(&requests),
        );
        let mut node = validator_dialing(&signer.id(), address).await;

        step_until(&mut node, |_| {
            logged(&logs, "Failed to get the authorized peers tree from ")
        })
        .await;

        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(node.held.tree().is_none());
        assert_eq!(
            allowed(&mut *shared(&node).lock().await),
            BTreeSet::from([signer.id()])
        );
    }

    #[tokio::test]
    async fn test_a_validator_asks_no_tree_of_a_peer_that_is_not_its_relayer() {
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        let address = listen(&mut peer).await;
        // The validator's relayer is another peer; it allows this one, as a
        // tree would.
        let mut node = validator_dialing(&random_peer(), address).await;
        shared(&node)
            .lock()
            .await
            .behaviour_mut()
            .allow_list
            .as_mut()
            .unwrap()
            .allow_peer(peer_id);

        let mut asked = false;
        let mut connected_since = None;
        drive(&mut node, &mut [peer], |node, _, event| {
            asked |= event
                .as_ref()
                .is_some_and(|(_, event)| is_tree_request(event));
            let connected = shared(node).try_lock().unwrap().is_connected(&peer_id);
            if connected && connected_since.is_none() {
                connected_since = Some(std::time::Instant::now());
            }
            connected_since.is_some_and(|since| since.elapsed() > Duration::from_millis(500))
        })
        .await;

        assert!(!asked);
    }

    // The relayer publishes `bytes` once the validator's subscription
    // reached it, and ignores the validator's requests, until `done`.
    async fn push_until<F>(
        node: &mut ValidatorNode,
        relayer: &mut Swarm<NodeBehaviour>,
        bytes: Vec<u8>,
        mut done: F,
    ) where
        F: FnMut(&mut ValidatorNode) -> bool,
    {
        let topic = gossipsub::IdentTopic::new("authorized_peers");
        let mut published = false;
        let peers = std::slice::from_mut(relayer);
        drive(node, peers, |node, peers, _| {
            if !published {
                published = peers[0]
                    .behaviour_mut()
                    .gossipsub
                    .publish(topic.clone(), bytes.clone())
                    .is_ok();
            }
            done(node)
        })
        .await;
    }

    #[tokio::test]
    async fn test_a_validator_accepts_a_pushed_tree_of_100_peers() {
        let (logs, _guard) = capture_logs();
        let mut signer = Signer::new();
        let peers: Vec<PeerId> = (0..100).map(|_| random_peer()).collect();
        let tree = signer.sign(&peers);
        let mut relayer = bare_swarm_of(&signer.keypair);
        let address = listen(&mut relayer).await;
        let mut node = validator_dialing(&signer.id(), address).await;

        push_until(&mut node, &mut relayer, wire_bytes(&tree), |node| {
            node.held.tree().is_some()
        })
        .await;

        let held = node.held.tree().unwrap();
        assert_eq!(held.peers.len(), 100);
        assert_eq!(held.timestamp, tree.timestamp);
        assert!(logged(&logs, "Accepted the authorized peers tree from "));
        // A pushed tree is no event.
        assert_eq!(node.base.metrics.messages_received, 0);
    }

    #[tokio::test]
    async fn test_a_tree_of_the_largest_size_travels_by_gossipsub() {
        let mut signer = Signer::new();
        let tree = signer.sign(&[random_peer()]);
        // JSON allows leading spaces: the same tree, as large as one may be.
        let encoded = wire_bytes(&tree);
        let mut bytes = vec![b' '; crate::utils::auth::MAX_WIRE_TREE_BYTES - encoded.len()];
        bytes.extend_from_slice(&encoded);
        let mut relayer = bare_swarm_of(&signer.keypair);
        let address = listen(&mut relayer).await;
        let mut node = validator_dialing(&signer.id(), address).await;

        push_until(&mut node, &mut relayer, bytes, |node| {
            node.held.tree().is_some()
        })
        .await;

        assert_eq!(node.held.tree().unwrap().timestamp, tree.timestamp);
    }

    #[tokio::test]
    async fn test_a_validator_refuses_a_pushed_tree_another_key_signed() {
        let (logs, _guard) = capture_logs();
        let relayer_keypair = generate_keypair(None);
        let mut relayer = bare_swarm_of(&relayer_keypair);
        let address = listen(&mut relayer).await;
        let mut node = validator_dialing(&relayer_keypair.public().to_peer_id(), address).await;
        let mut impostor = Signer::new();
        let forged = impostor.sign(&[random_peer()]);

        push_until(&mut node, &mut relayer, wire_bytes(&forged), |_| {
            logged(&logs, "Refused the authorized peers tree from ")
        })
        .await;

        assert!(node.held.tree().is_none());
        assert!(logged(&logs, "Refused the authorized peers tree from "));
    }

    #[tokio::test]
    async fn test_a_validator_denies_an_outsider_admits_it_once_authorized_and_cuts_it_off() {
        let (logs, _guard) = capture_logs();
        let mut signer = Signer::new();
        let relayer_id = signer.id();
        let relayer = bare_swarm_of(&signer.keypair);
        let outsider = bare_swarm().await;
        let outsider_id = *outsider.local_peer_id();
        let mut node = validator_of(&relayer_id).await;
        let validator = listen(&mut *shared(&node).lock().await).await;
        let mut peers = [relayer, outsider];

        // Before any tree, the outsider is denied.
        peers[1].dial(validator.clone()).unwrap();
        let denial = format!("Denied a connection from unauthorized peer {}", outsider_id);
        drive(&mut node, &mut peers, |_, _, _| logged(&logs, &denial)).await;

        // The relayer answers the validator's pull with a tree that
        // authorizes the outsider.
        let first = signer.sign(&[outsider_id]);
        let relayer_address = listen(&mut peers[0]).await;
        shared(&node).lock().await.dial(relayer_address).unwrap();
        drive(&mut node, &mut peers, |node, peers, event| {
            if let Some((
                0,
                SwarmEvent::Behaviour(NodeBehaviourEvent::AuthorizedPeers(
                    request_response::Event::Message {
                        message: request_response::Message::Request { channel, .. },
                        ..
                    },
                )),
            )) = event
            {
                peers[0]
                    .behaviour_mut()
                    .authorized_peers
                    .send_response(channel, WireTree::from(&first))
                    .unwrap();
            }
            node.held.tree().is_some()
        })
        .await;

        // Now the outsider connects.
        peers[1].dial(validator).unwrap();
        drive(&mut node, &mut peers, |node, _, _| {
            shared(node).try_lock().unwrap().is_connected(&outsider_id)
        })
        .await;

        // A pushed revocation cuts it off.
        let revoked = wire_bytes(&signer.sign(&[]));
        let topic = gossipsub::IdentTopic::new("authorized_peers");
        let mut published = false;
        let closed = format!("Connection closed with {}", outsider_id);
        drive(&mut node, &mut peers, |node, peers, _| {
            if !published {
                published = peers[0]
                    .behaviour_mut()
                    .gossipsub
                    .publish(topic.clone(), revoked.clone())
                    .is_ok();
            }
            !shared(node).try_lock().unwrap().is_connected(&outsider_id) && logged(&logs, &closed)
        })
        .await;
        assert_eq!(
            allowed(&mut *shared(&node).lock().await),
            BTreeSet::from([relayer_id])
        );
    }
}
