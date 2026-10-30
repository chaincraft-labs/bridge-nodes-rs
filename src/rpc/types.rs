use jsonrpsee::types::error::ErrorObject;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DistributeKeysParams {
    pub peer_ids: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("Invalid peer ID: {0}")]
    InvalidPeerId(String),

    #[error("No peers provided")]
    NoPeers,

    #[error("Distribution failed: {0}")]
    DistributionFailed(String),

    #[error("Authorization failed: {0}")]
    AuthorizationFailed(String),

    #[error("The relayer is not running")]
    RelayerStopped,
}

impl From<RpcError> for ErrorObject<'static> {
    fn from(err: RpcError) -> Self {
        ErrorObject::owned(1, err.to_string(), None::<()>)
    }
}
