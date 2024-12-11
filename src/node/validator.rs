use futures::StreamExt;
use libp2p::{identity::Keypair, swarm::SwarmEvent};
use libp2p::{gossipsub, request_response};
use tokio::sync::Mutex;
use std::{sync::Arc, time::Duration};
use tracing::{error, info};

use super::common::{BaseNode, NodeCommon};
use crate::{PartialKeyRequest, PartialKeyResponse};
use crate::{types::{NodeBehaviourEvent, NodeConfig, NodeError}, CustomEvent, NodeBehaviour, NodeMetrics, NodeState};

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
            base: BaseNode::new(config)
        }
    }

    async fn init_gossipsub_topic(&mut self) -> Result<gossipsub::IdentTopic, NodeError> {
        let topic = gossipsub::IdentTopic::new("custom_events");

        let swarm = self.base.swarm.as_ref()
            .ok_or(NodeError::SwarmNotInitialized)?;
        let mut swarm = swarm.lock().await;

        swarm.behaviour_mut().gossipsub.subscribe(&topic)
            .map_err(|e| NodeError::ConfigError(e.to_string()))?;

        info!("Subscribed to topic: {}", topic);

        Ok(topic)
    }

    async fn handle_key_exchange_event(&mut self, event: request_response::Event<PartialKeyRequest, PartialKeyResponse>) {
        match event {
            request_response::Event::Message { peer, message } => {
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

                            info!("Sending key response to relayer for index {}", request.node_index);
                            if let Err(e) = swarm.behaviour_mut().key_exchange.send_response(channel, response) {
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
                error!("Failed to receive request from relayer {}: {:?}", peer, error);
            }
            request_response::Event::ResponseSent { peer, request_id } => {
                info!("Successfully sent response to relayer {} for request {}", peer, request_id);
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

        let swarm = self.base.swarm.as_ref()
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

                if let Err(e) = behaviour.gossipsub.publish(topic.clone(), message.as_bytes()) {
                    error!("Failed to publish event: {:?}", e);
                } else {
                    info!("Event published successfully");
                }
            }
        });

        Ok(())
    }

    // async fn store_partial_key(&mut self, response: PartialKeyResponse) {
    //     // Implémenter le stockage sécurisé de la clé partielle
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
                self.base.handle_common_events(event).await;
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

        let swarm = Arc::clone(self.base.swarm.as_ref()
            .expect("Swarm should be initialized"));

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