use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub path: Vec<[u8; 32]>,
    pub position: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizedPeersTree {
    pub root: [u8; 32],
    pub peers: BTreeMap<PeerId, MerkleProof>,
    pub timestamp: i64,
    pub signature: Vec<u8>,
}
