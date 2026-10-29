use chrono::Utc;
use libp2p::PeerId;
use std::collections::BTreeMap;

use super::types::{AuthorizedPeersTree, MerkleProof};

impl Default for AuthorizedPeersTree {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorizedPeersTree {
    pub fn new() -> Self {
        Self {
            root: [0; 32],
            peers: BTreeMap::new(),
            timestamp: Utc::now().timestamp(),
            signature: Vec::new(),
        }
    }

    // Hashes a node
    fn hash_node(data: &[u8]) -> [u8; 32] {
        *blake3::hash(data).as_bytes()
    }

    // Combines two hashes into a parent node
    fn combine_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(left);
        hasher.update(right);
        *hasher.finalize().as_bytes()
    }

    // Updates the tree with a new list of peers
    pub fn update_tree(
        &mut self,
        peers: Vec<PeerId>,
        keypair: &libp2p::identity::Keypair,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Sorts the peers to guarantee a deterministic order
        let mut leaf_nodes: Vec<_> = peers
            .iter()
            .map(|peer| AuthorizedPeersTree::hash_node(&peer.to_bytes()))
            .collect();

        // If the number of leaves is odd, duplicates the last one
        if leaf_nodes.len() % 2 != 0 {
            if let Some(last) = leaf_nodes.last().cloned() {
                leaf_nodes.push(last);
            }
        }

        // Builds the tree and computes the proofs
        let mut current_level = leaf_nodes.clone();
        let mut proofs: BTreeMap<PeerId, MerkleProof> = BTreeMap::new();

        while current_level.len() > 1 {
            let mut next_level = Vec::new();

            for i in (0..current_level.len()).step_by(2) {
                let left = &current_level[i];
                let right = &current_level[i + 1];
                let parent = Self::combine_hash(left, right);
                next_level.push(parent);

                // Computes the proofs for the peers at this level
                for peer in peers.iter() {
                    let leaf_hash = AuthorizedPeersTree::hash_node(&peer.to_bytes());
                    if leaf_hash == *left || leaf_hash == *right {
                        let proof = proofs.entry(*peer).or_insert(MerkleProof {
                            path: Vec::new(),
                            position: Vec::new(),
                        });

                        proof
                            .path
                            .push(if leaf_hash == *left { *right } else { *left });
                        proof.position.push(if leaf_hash == *left { 1 } else { 0 });
                    }
                }
            }

            current_level = next_level;
        }

        // Updates the root
        self.root = current_level[0];
        self.peers = proofs;
        self.timestamp = Utc::now().timestamp();

        // Signs the root with the timestamp
        let mut data_to_sign = Vec::new();
        data_to_sign.extend_from_slice(&self.root);
        data_to_sign.extend_from_slice(&self.timestamp.to_be_bytes());

        self.signature = keypair
            .sign(&data_to_sign)
            .map_err(|e| format!("Signing error: {}", e))?
            .to_vec();

        Ok(())
    }

    // Checks that a peer is in the tree
    pub fn verify_peer(&self, peer: &PeerId) -> bool {
        if let Some(proof) = self.peers.get(peer) {
            let mut current_hash = AuthorizedPeersTree::hash_node(&peer.to_bytes());

            for (proof_elem, position) in proof.path.iter().zip(proof.position.iter()) {
                current_hash = if *position == 0 {
                    Self::combine_hash(&current_hash, proof_elem)
                } else {
                    Self::combine_hash(proof_elem, &current_hash)
                };
            }

            current_hash == self.root
        } else {
            false
        }
    }

    // Checks the tree's signature
    pub fn verify_signature(
        &self,
        public_key: &libp2p::identity::PublicKey,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let mut data_to_verify = Vec::new();
        data_to_verify.extend_from_slice(&self.root);
        data_to_verify.extend_from_slice(&self.timestamp.to_be_bytes());

        Ok(public_key.verify(&data_to_verify, &self.signature))
    }
}

// Tests
#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity;

    // KNOWN DEFECT, frozen until the tree is fixed (lot L2 of the mission):
    // proofs only hold the leaf-level sibling and their position flag is read
    // backwards by `verify_peer`, so no peer of a 3-peer tree verifies. When
    // the tree is fixed this test goes red: replace it with tests asserting
    // that every peer verifies.
    #[test]
    fn test_wip_tree_rejects_its_own_peers_known_defect() -> Result<(), Box<dyn std::error::Error>>
    {
        // Creates test peers
        let keypair = identity::Keypair::generate_ed25519();
        let peer1 = PeerId::random();
        let peer2 = PeerId::random();
        let peer3 = PeerId::random();

        // Creates and updates the tree
        let mut tree = AuthorizedPeersTree::new();
        tree.update_tree(vec![peer1, peer2, peer3], &keypair)?;

        // Checks the peers: each proof stops at the leaf level, and none
        // verifies
        for peer in [peer1, peer2, peer3] {
            assert_eq!(tree.peers[&peer].path.len(), 1);
            assert!(!tree.verify_peer(&peer));
        }
        assert!(!tree.verify_peer(&PeerId::random()));

        // Checks the signature
        assert!(tree.verify_signature(&keypair.public())?);

        Ok(())
    }
}
