use blake3;
use chrono::Utc;
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub path: Vec<[u8; 32]>,
    pub position: Vec<usize>, // 0 pour gauche, 1 pour droite
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizedPeersTree {
    pub root: [u8; 32],
    pub peers: BTreeMap<PeerId, MerkleProof>,
    pub timestamp: i64,
    pub signature: Vec<u8>,
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

    // Calcule le hash d'un nœud
    fn hash_node(data: &[u8]) -> [u8; 32] {
        *blake3::hash(data).as_bytes()
    }

    // Combine deux hash pour former un nœud parent
    fn combine_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(left);
        hasher.update(right);
        *hasher.finalize().as_bytes()
    }

    // Met à jour l'arbre avec une nouvelle liste de peers
    pub fn update_tree(
        &mut self,
        peers: Vec<PeerId>,
        keypair: &libp2p::identity::Keypair,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Trie les peers pour garantir un ordre déterministe
        let mut leaf_nodes: Vec<_> = peers
            .iter()
            .map(|peer| AuthorizedPeersTree::hash_node(&peer.to_bytes()))
            .collect();

        // Si le nombre de feuilles est impair, duplique la dernière
        if leaf_nodes.len() % 2 != 0 {
            if let Some(last) = leaf_nodes.last().cloned() {
                leaf_nodes.push(last);
            }
        }

        // Construit l'arbre et calcule les preuves
        let mut current_level = leaf_nodes.clone();
        let mut proofs: BTreeMap<PeerId, MerkleProof> = BTreeMap::new();
        let mut level_start = 0;

        while current_level.len() > 1 {
            let mut next_level = Vec::new();

            for i in (0..current_level.len()).step_by(2) {
                let left = &current_level[i];
                let right = &current_level[i + 1];
                let parent = Self::combine_hash(left, right);
                next_level.push(parent);

                // Calcule les preuves pour les peers à ce niveau
                for (idx, peer) in peers.iter().enumerate() {
                    let leaf_hash = AuthorizedPeersTree::hash_node(&peer.to_bytes());
                    if leaf_hash == *left || leaf_hash == *right {
                        let mut proof = proofs.entry(*peer).or_insert(MerkleProof {
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

            level_start += current_level.len();
            current_level = next_level;
        }

        // Met à jour le root
        self.root = current_level[0];
        self.peers = proofs;
        self.timestamp = Utc::now().timestamp();

        // Signe le root avec le timestamp
        let mut data_to_sign = Vec::new();
        data_to_sign.extend_from_slice(&self.root);
        data_to_sign.extend_from_slice(&self.timestamp.to_be_bytes());

        self.signature = keypair
            .sign(&data_to_sign)
            .map_err(|e| format!("Signing error: {}", e))?
            .to_vec();

        Ok(())
    }

    // Vérifie qu'un peer est dans l'arbre
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

    // Vérifie la signature de l'arbre
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

    #[test]
    fn test_merkle_tree_update_and_verify() -> Result<(), Box<dyn std::error::Error>> {
        // Crée des peers de test
        let keypair = identity::Keypair::generate_ed25519();
        let peer1 = PeerId::random();
        let peer2 = PeerId::random();
        let peer3 = PeerId::random();

        // Crée et met à jour l'arbre
        let mut tree = AuthorizedPeersTree::new();
        tree.update_tree(vec![peer1, peer2, peer3], &keypair)?;

        // Vérifie les peers
        assert!(tree.verify_peer(&peer1));
        assert!(tree.verify_peer(&peer2));
        assert!(tree.verify_peer(&peer3));
        assert!(!tree.verify_peer(&PeerId::random()));

        // Vérifie la signature
        assert!(tree.verify_signature(&keypair.public())?);

        Ok(())
    }
}
