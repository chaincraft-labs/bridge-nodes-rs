use serde::{Deserialize, Serialize};
use jsonrpsee::types::error::ErrorObject;

#[derive(Debug, Serialize, Deserialize)]
pub struct DistributeKeysParams {
    pub peer_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("Invalid peer ID format")]
    InvalidPeerId,

    #[error("No peers provided")]
    NoPeers,

    #[error("Distribution failed: {0}")]
    DistributionFailed(String),
}

impl From<RpcError> for ErrorObject<'static> {
    fn from(err: RpcError) -> Self {
        ErrorObject::owned(
            1,
            err.to_string(),
            None::<()>
        )
    }
}