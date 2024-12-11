use std::sync::Arc;
use std::str::FromStr;
use jsonrpsee::{
    core::{async_trait, RpcResult},
    proc_macros::rpc,
    server::{ServerBuilder, ServerHandle},
};
use libp2p::PeerId;
use tokio::sync::Mutex;
use tracing::info;

use crate::node::relayer::RelayerNode;
use crate::types::NodeError;
use super::types::RpcError;

#[rpc(server, client)]
pub trait RpcApi {
    #[method(name = "distribute_keys")]
    async fn distribute_keys(&self, peer_ids: Vec<String>) -> RpcResult<String>;
}

pub struct RpcServerImpl {
    relayer: Arc<Mutex<RelayerNode>>,
}

#[async_trait]
impl RpcApiServer for RpcServerImpl {
    async fn distribute_keys(&self, peer_ids: Vec<String>) -> RpcResult<String> { // Et ici

        let peer_ids = peer_ids
            .iter()
            .filter_map(|id| PeerId::from_str(id).ok())
            .collect::<Vec<_>>();

        if peer_ids.is_empty() {
            return Err(RpcError::NoPeers.into());
        }

        let mut relayer = self.relayer.lock().await;

        match relayer.distribute_keys(peer_ids).await {
            Ok(_) => {
                Ok("Key distribution started successfully".to_string())
            }
            Err(e) => {
                Err(RpcError::DistributionFailed(e.to_string()).into())
            }
        }
    }
}

pub async fn start_rpc_server(relayer: Arc<Mutex<RelayerNode>>, addr: &str) -> Result<ServerHandle, NodeError> {
    let server = ServerBuilder::default()
        .build(addr)
        .await
        .map_err(|e| NodeError::ConfigError(format!("Failed to build RPC server: {}", e)))?;

    let rpc = RpcServerImpl { relayer };

    let handle = server.start(rpc.into_rpc());
    info!("RPC server started on {}", addr);
    Ok(handle)
}