use futures::StreamExt;
use libp2p::{request_response, swarm::SwarmEvent};
use libp2p::{gossipsub, PeerId};
use libp2p::identity::Keypair;
use tokio::sync::Mutex;
use std::sync::Arc;
use tracing::{error, info};

use super::common::{BaseNode, NodeCommon};
use crate::rpc::start_rpc_server;
use crate::{PartialKeyRequest, PartialKeyResponse};
use crate::{types::{NodeBehaviourEvent, NodeConfig, NodeError, NodeMetrics, NodeState}, NodeBehaviour};


#[derive(Clone)]
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

    pub async fn distribute_keys(&mut self, peers: Vec<PeerId>) -> Result<(), NodeError> {

        for (index, peer_id) in peers.iter().enumerate() {

            let swarm = self.base.swarm.as_ref()
                .ok_or(NodeError::SwarmNotInitialized)?;

            let mut swarm = swarm.lock().await;
            if !swarm.is_connected(peer_id) {
                return Err(NodeError::PeerError(
                    format!("Peer {} not connected. Ensure peer is connected before distributing keys", peer_id)
                ));
            }

            let request = PartialKeyRequest {
                node_index: index,
            };

            swarm.behaviour_mut().key_exchange.send_request(peer_id, request);
        }

        Ok(())
    }

    async fn handle_relayer_events(&mut self, event: SwarmEvent<NodeBehaviourEvent>) {
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
                    request_response::Event::Message {
                        peer,
                        message,
                    } => match message {
                        request_response::Message::Request {
                            request_id,
                            request,
                            channel,
                        } => {
                            info!("Relayer received request from {} with id {} for index {}", peer, request_id, request.node_index);
                            if let Some(swarm) = &self.base.swarm {
                                let mut swarm = swarm.lock().await;
                                let response = PartialKeyResponse {
                                    key_data: vec![],
                                    node_index: request.node_index,
                                };
                                let _ = swarm.behaviour_mut().key_exchange.send_response(channel, response);
                            }
                        }
                        // Relayer should not receive responses
                        request_response::Message::Response {
                            request_id,
                            response,
                        } => {
                            info!("Relayer received response from {} with id {} response {:?}", peer, request_id, response);
                        }
                    },
                    request_response::Event::OutboundFailure {
                        peer,
                        error,
                        ..
                    } => {
                        error!("OutboundFailure => Failed to send key to validator {}: {:?}", peer, error);
                        // Error while sending message
                        // Retry here
                    },
                    request_response::Event::InboundFailure {..} => {
                        // Ignore because the relayer should not receive requests
                    },
                    request_response::Event::ResponseSent {
                        peer,
                        request_id,
                    } => {
                        info!(
                            "Successfully sent key to validator {}, request_id: {}",
                            peer, request_id
                        );
                    }
                }
            }

            _ => {
                // Handle common events through the base implementation
                self.base.handle_common_events(event).await;
            }
        }
    }

    pub async fn run(&mut self) -> Result<(), NodeError> {
        info!("🌟 Relayer node");
        self.base.init_swarm().await?;
        self.base.init_network_listener().await?;
        self.base.state = NodeState::Connected;
        self.init_gossipsub_topic().await?;

        // Start RPC server
        // port 8545
        let rpc_addr_formatted = format!("{}:8545", self.base.config.listen_address);
        let rpc_addr: &str = &rpc_addr_formatted; // or configurable via NodeConfig
        let relayer = Arc::new(Mutex::new(self.clone()));
        let _rpc_handle = start_rpc_server(relayer, rpc_addr).await?;

        let swarm = Arc::clone(self.base.swarm.as_ref()
            .expect("Swarm should be initialized"));

        while self.base.state != NodeState::Disconnected {
            let event = {
                let mut locked_swarm = swarm.lock().await;
                locked_swarm.next().await
            };

            if let Some(event) = event {
                self.handle_relayer_events(event).await;
            }
        }

        Ok(())
    }
}