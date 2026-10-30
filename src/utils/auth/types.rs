use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MerkleProof {
    /// Sibling hashes from the leaf up to the root, one per level where the
    /// node being verified has a sibling.
    pub path: Vec<[u8; 32]>,
    /// Where the node being verified sits at step `i`: `0` when it is the
    /// left child (the sibling `path[i]` is on its right), `1` when it is the
    /// right child (the sibling is on its left). Any other value makes the
    /// proof invalid.
    pub position: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizedPeersTree {
    pub root: [u8; 32],
    pub peers: BTreeMap<PeerId, MerkleProof>,
    pub timestamp: i64,
    pub signature: Vec<u8>,
}
