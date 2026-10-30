use futures::StreamExt;
use libp2p::{gossipsub, request_response};
use libp2p::{identity::Keypair, swarm::SwarmEvent};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tracing::{error, info};

use super::common::{BaseNode, NodeCommon};
use crate::{
    types::{NodeBehaviourEvent, NodeConfig, NodeError},
    CustomEvent, NodeBehaviour, NodeMetrics, NodeState,
};
use crate::{PartialKeyRequest, PartialKeyResponse};

#[derive(Clone)]
pub struct ValidatorNode {
    base: BaseNode,
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

        let topic = gossipsub::IdentTopic::new("custom_events");
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

            _ => {
                // Handle common events through the base implementation
                if let Some(swarm) = self.base.swarm.clone() {
                    let mut swarm = swarm.lock().await;
                    self.base.handle_common_events(&mut swarm, event).await;
                }
            }
        }
    }

    pub async fn run(&mut self) -> Result<(), NodeError> {
        info!("🔶 Validator node");
        self.base.init_swarm().await?;
        self.base.init_network_listener().await?;
        self.base.connect_to_bootstrap().await?;
        self.base.state = NodeState::Connected;
        self.init_gossipsub_topic().await?;
        self.publish_blockchain_event().await?;

        let swarm = Arc::clone(
            self.base
                .swarm
                .as_ref()
                .expect("Swarm should be initialized"),
        );

        while self.base.state != NodeState::Disconnected {
            let event = {
                let mut locked_swarm = swarm.lock().await;
                locked_swarm.next().await
            };

            if let Some(event) = event {
                self.handle_validator_events(event).await;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::common::test_support::{bare_swarm, kademlia_addresses, listen, test_config};
    use futures::StreamExt;
    use libp2p::Swarm;

    // A validator subscribed to its topic, its swarm shared as `run` shares
    // it, dialing a peer the test drives.
    async fn validator_dialing(address: libp2p::Multiaddr) -> ValidatorNode {
        let mut node = ValidatorNode::new(test_config(false));
        node.base.init_swarm().await.unwrap();
        node.init_gossipsub_topic().await.unwrap();
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

    // Runs the validator's handler on every validator event, never holding
    // the swarm lock while it does, and hands every peer event, and a tick
    // every 100 ms, to `step` until it returns true.
    async fn drive<F>(node: &mut ValidatorNode, peer: &mut Swarm<NodeBehaviour>, mut step: F)
    where
        F: FnMut(
            &mut ValidatorNode,
            &mut Swarm<NodeBehaviour>,
            Option<SwarmEvent<NodeBehaviourEvent>>,
        ) -> bool,
    {
        let swarm = Arc::clone(node.base.swarm.as_ref().unwrap());
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (validator_event, peer_event) = {
                    let mut validator = swarm.lock().await;
                    tokio::select! {
                        event = validator.select_next_some() => (Some(event), None),
                        event = peer.select_next_some() => (None, Some(event)),
                        _ = tick.tick() => (None, None),
                    }
                };
                if let Some(event) = validator_event {
                    node.handle_validator_events(event).await;
                }
                if step(node, peer, peer_event) {
                    return;
                }
            }
        })
        .await
        .expect("the validator did not handle the peer in time");
    }

    #[tokio::test]
    async fn test_validator_answers_a_key_request_with_its_partial_key() {
        let mut peer = bare_swarm().await;
        let address = listen(&mut peer).await;
        let mut node = validator_dialing(address).await;
        let validator_id = node.base.keypair.as_ref().unwrap().public().to_peer_id();

        let mut answer = None;
        drive(&mut node, &mut peer, |_, peer, event| {
            match event {
                Some(SwarmEvent::ConnectionEstablished { peer_id, .. })
                    if peer_id == validator_id =>
                {
                    peer.behaviour_mut()
                        .key_exchange
                        .send_request(&validator_id, PartialKeyRequest { node_index: 3 });
                }
                Some(SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(
                    request_response::Event::Message {
                        message: request_response::Message::Response { response, .. },
                        ..
                    },
                ))) => answer = Some(response),
                Some(SwarmEvent::Behaviour(NodeBehaviourEvent::KeyExchange(
                    request_response::Event::OutboundFailure { error, .. },
                ))) => panic!("the validator did not answer: {:?}", error),
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
        let address = listen(&mut peer).await;
        let mut node = validator_dialing(address).await;
        let topic = gossipsub::IdentTopic::new("custom_events");

        let mut published = false;
        drive(&mut node, &mut peer, |node, peer, _| {
            // Publishing fails until the peer learns the validator's topic.
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
    async fn test_validator_routes_the_peer_it_connected_to() {
        let mut peer = bare_swarm().await;
        let peer_id = *peer.local_peer_id();
        let address = listen(&mut peer).await;
        let mut node = validator_dialing(address).await;

        drive(&mut node, &mut peer, |node, _, _| {
            let swarm = Arc::clone(node.base.swarm.as_ref().unwrap());
            let mut swarm = swarm.try_lock().unwrap();
            !kademlia_addresses(&mut swarm, &peer_id).is_empty()
        })
        .await;
    }
}
