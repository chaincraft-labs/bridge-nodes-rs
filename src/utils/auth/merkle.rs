use chrono::Utc;
use libp2p::PeerId;
use std::collections::BTreeMap;

use super::types::{AuthorizedPeersTree, MerkleProof};

/// Prefix of a leaf hash, so a leaf can never pass for an internal node.
const LEAF_PREFIX: u8 = 0x00;
/// Prefix of an internal node hash.
const NODE_PREFIX: u8 = 0x01;
/// Tag of the signed message, so a relayer signature over the list is valid
/// for no other message the same key signs.
const SIGNATURE_TAG: &[u8] = b"chaincraft/authorized-peers/v1";
/// Root of the tree over no peer.
const EMPTY_ROOT: [u8; 32] = [0; 32];

impl Default for AuthorizedPeersTree {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthorizedPeersTree {
    pub fn new() -> Self {
        Self {
            root: EMPTY_ROOT,
            peers: BTreeMap::new(),
            timestamp: Utc::now().timestamp(),
            signature: Vec::new(),
        }
    }

    // Hashes a leaf: blake3(0x00 || peer_id)
    fn hash_leaf(peer: &PeerId) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&[LEAF_PREFIX]);
        hasher.update(&peer.to_bytes());
        *hasher.finalize().as_bytes()
    }

    // Combines two hashes into a parent node: blake3(0x01 || left || right)
    fn combine_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&[NODE_PREFIX]);
        hasher.update(left);
        hasher.update(right);
        *hasher.finalize().as_bytes()
    }

    // The bytes the relayer signs: tag || root || timestamp (big endian)
    fn signed_message(root: &[u8; 32], timestamp: i64) -> Vec<u8> {
        let mut message = Vec::with_capacity(SIGNATURE_TAG.len() + 32 + 8);
        message.extend_from_slice(SIGNATURE_TAG);
        message.extend_from_slice(root);
        message.extend_from_slice(&timestamp.to_be_bytes());
        message
    }

    // Builds the level above `level`: nodes are paired left to right, and an
    // unpaired last node is carried up unchanged.
    fn next_level(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
        level
            .chunks(2)
            .map(|pair| match pair {
                [left, right] => Self::combine_hash(left, right),
                [single] => *single,
                _ => unreachable!("chunks(2) yields one or two nodes"),
            })
            .collect()
    }

    // The timestamp of the next signed tree: now, but always strictly after
    // the previous one, so a receiver can refuse an older tree even when two
    // updates fall in the same second or the clock went back.
    fn next_timestamp(previous: i64, now: i64) -> i64 {
        now.max(previous.saturating_add(1))
    }

    // The root and the proofs of the tree over `peers`, which must be sorted
    // and without duplicates.
    fn build(peers: &[PeerId]) -> ([u8; 32], BTreeMap<PeerId, MerkleProof>) {
        let mut levels: Vec<Vec<[u8; 32]>> = vec![peers.iter().map(Self::hash_leaf).collect()];
        while let Some(level) = levels.last().filter(|level| level.len() > 1) {
            let next = Self::next_level(level);
            levels.push(next);
        }

        let mut proofs = BTreeMap::new();
        for (leaf_index, peer) in peers.iter().enumerate() {
            let mut proof = MerkleProof {
                path: Vec::new(),
                position: Vec::new(),
            };
            let mut index = leaf_index;
            for level in &levels {
                let sibling = index ^ 1;
                // A node without a sibling (carried up, or the root) adds no
                // proof step
                if let Some(hash) = level.get(sibling) {
                    proof.path.push(*hash);
                    proof.position.push(index % 2);
                }
                index /= 2;
            }
            proofs.insert(*peer, proof);
        }

        let root = levels
            .last()
            .and_then(|level| level.first())
            .copied()
            .unwrap_or(EMPTY_ROOT);
        (root, proofs)
    }

    // Updates the tree with a new list of peers, then signs it. The timestamp
    // is strictly greater than the tree's previous one.
    pub fn update_tree(
        &mut self,
        peers: Vec<PeerId>,
        keypair: &libp2p::identity::Keypair,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Deduplicates and sorts the peers so that the same set always yields
        // the same root
        let mut peers = peers;
        peers.sort();
        peers.dedup();

        let (root, proofs) = Self::build(&peers);
        self.root = root;
        self.peers = proofs;
        self.timestamp = Self::next_timestamp(self.timestamp, Utc::now().timestamp());

        // Signs the tagged root and timestamp
        self.signature = keypair
            .sign(&Self::signed_message(&self.root, self.timestamp))
            .map_err(|e| format!("Signing error: {}", e))?;

        Ok(())
    }

    // Checks that a peer is in the tree. The proof may come from the network:
    // a malformed one yields `false`, never a panic.
    pub fn verify_peer(&self, peer: &PeerId) -> bool {
        let Some(proof) = self.peers.get(peer) else {
            return false;
        };
        if proof.path.len() != proof.position.len() {
            return false;
        }

        let mut current_hash = Self::hash_leaf(peer);
        for (sibling, position) in proof.path.iter().zip(proof.position.iter()) {
            current_hash = match position {
                0 => Self::combine_hash(&current_hash, sibling),
                1 => Self::combine_hash(sibling, &current_hash),
                _ => return false,
            };
        }

        current_hash == self.root
    }

    // Checks that the root and every proof are those of the tree over the
    // peers it lists. A signature covers the root only: a tree read from disk
    // or the network whose peers were edited still carries a valid one.
    pub fn is_consistent(&self) -> bool {
        let peers: Vec<PeerId> = self.peers.keys().copied().collect();
        let (root, proofs) = Self::build(&peers);
        root == self.root && proofs == self.peers
    }

    // Checks the tree's signature
    pub fn verify_signature(
        &self,
        public_key: &libp2p::identity::PublicKey,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(public_key.verify(
            &Self::signed_message(&self.root, self.timestamp),
            &self.signature,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::peer_id::{generate_keypair, seed_phrase_to_bytes};
    use libp2p::identity::Keypair;
    use std::str::FromStr;

    const SIZES: [usize; 7] = [0, 1, 2, 3, 5, 8, 9];

    // Root of the tree over the peers of the seeds `relayer_test` and
    // `validator_test`. A change of hashing, ordering or prefixes moves it.
    const KNOWN_ROOT_HEX: &str = "4c7899a581576f2349c4c551cb5e079298e353b7c649c9694df194d75bcf9d17";

    fn random_peers(count: usize) -> Vec<PeerId> {
        (0..count).map(|_| PeerId::random()).collect()
    }

    fn tree_over(peers: &[PeerId], keypair: &Keypair) -> AuthorizedPeersTree {
        let mut tree = AuthorizedPeersTree::new();
        tree.update_tree(peers.to_vec(), keypair).unwrap();
        tree
    }

    fn sorted(peers: &[PeerId]) -> Vec<PeerId> {
        let mut peers = peers.to_vec();
        peers.sort();
        peers
    }

    fn leaf(peer: &PeerId) -> [u8; 32] {
        let mut data = vec![0x00];
        data.extend_from_slice(&peer.to_bytes());
        *blake3::hash(&data).as_bytes()
    }

    fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut data = vec![0x01];
        data.extend_from_slice(left);
        data.extend_from_slice(right);
        *blake3::hash(&data).as_bytes()
    }

    #[test]
    fn test_every_peer_verifies_and_an_outsider_does_not() {
        let keypair = Keypair::generate_ed25519();
        for count in SIZES {
            let peers = random_peers(count);
            let tree = tree_over(&peers, &keypair);

            assert_eq!(tree.peers.len(), count, "{count} peers");
            for peer in &peers {
                assert!(tree.verify_peer(peer), "{count} peers: {peer} must verify");
            }
            assert!(!tree.verify_peer(&PeerId::random()), "{count} peers");
            assert!(tree.verify_signature(&keypair.public()).unwrap());
        }
    }

    #[test]
    fn test_empty_tree_has_a_zero_root_no_proof_and_is_signed() {
        let keypair = Keypair::generate_ed25519();
        let tree = tree_over(&[], &keypair);

        assert_eq!(tree.root, [0u8; 32]);
        assert!(tree.peers.is_empty());
        assert!(!tree.signature.is_empty());
        assert!(tree.verify_signature(&keypair.public()).unwrap());
        assert!(!tree.verify_peer(&PeerId::random()));
    }

    #[test]
    fn test_new_and_default_trees_are_empty() {
        for tree in [AuthorizedPeersTree::new(), AuthorizedPeersTree::default()] {
            assert_eq!(tree.root, [0u8; 32]);
            assert!(tree.peers.is_empty());
            assert!(tree.signature.is_empty());
        }
    }

    #[test]
    fn test_single_peer_root_is_its_leaf_and_its_proof_is_empty() {
        let peer = PeerId::random();
        let tree = tree_over(&[peer], &Keypair::generate_ed25519());

        assert_eq!(tree.root, leaf(&peer));
        assert!(tree.peers[&peer].path.is_empty());
        assert!(tree.peers[&peer].position.is_empty());
    }

    #[test]
    fn test_root_follows_the_documented_layout() {
        let keypair = Keypair::generate_ed25519();

        // Two peers: node(leaf0, leaf1), leaves in PeerId order
        let p = sorted(&random_peers(2));
        let tree = tree_over(&[p[1], p[0]], &keypair);
        assert_eq!(tree.root, node(&leaf(&p[0]), &leaf(&p[1])));

        // Three peers: the third leaf is carried up, never duplicated
        let p = sorted(&random_peers(3));
        let l: Vec<_> = p.iter().map(leaf).collect();
        let tree = tree_over(&p, &keypair);
        assert_eq!(tree.root, node(&node(&l[0], &l[1]), &l[2]));

        // Five peers: the fifth leaf is carried up twice
        let p = sorted(&random_peers(5));
        let l: Vec<_> = p.iter().map(leaf).collect();
        let tree = tree_over(&p, &keypair);
        let left = node(&node(&l[0], &l[1]), &node(&l[2], &l[3]));
        assert_eq!(tree.root, node(&left, &l[4]));
    }

    #[test]
    fn test_proofs_hold_one_step_per_sibling_with_its_position() {
        let p = sorted(&random_peers(5));
        let l: Vec<_> = p.iter().map(leaf).collect();
        let tree = tree_over(&p, &Keypair::generate_ed25519());

        // Leaf 1 is a right child, then a left child, then a left child
        let proof = &tree.peers[&p[1]];
        let left = node(&l[0], &l[1]);
        let right = node(&l[2], &l[3]);
        assert_eq!(proof.path, vec![l[0], right, l[4]]);
        assert_eq!(proof.position, vec![1, 0, 0]);

        // Leaf 2: left, then right, then left
        let proof = &tree.peers[&p[2]];
        assert_eq!(proof.path, vec![l[3], left, l[4]]);
        assert_eq!(proof.position, vec![0, 1, 0]);

        // Leaf 4 is carried twice: those steps add nothing to its proof
        let proof = &tree.peers[&p[4]];
        assert_eq!(proof.path, vec![node(&left, &right)]);
        assert_eq!(proof.position, vec![1]);
    }

    #[test]
    fn test_root_does_not_depend_on_order_or_duplicates() {
        let keypair = Keypair::generate_ed25519();
        let peers = random_peers(5);
        let reference = tree_over(&peers, &keypair);

        let mut reversed = peers.clone();
        reversed.reverse();
        let mut duplicated = peers.clone();
        duplicated.extend_from_slice(&peers[1..3]);
        duplicated.push(peers[0]);
        duplicated.rotate_left(2);

        for input in [reversed, duplicated] {
            let tree = tree_over(&input, &keypair);
            assert_eq!(tree.root, reference.root);
            assert_eq!(
                tree.peers.keys().collect::<Vec<_>>(),
                reference.peers.keys().collect::<Vec<_>>()
            );
            for peer in &peers {
                assert!(tree.verify_peer(peer));
            }
        }
    }

    #[test]
    fn test_tampered_path_element_fails() {
        let peers = random_peers(9);
        let tree = tree_over(&peers, &Keypair::generate_ed25519());

        for peer in &peers {
            for step in 0..tree.peers[peer].path.len() {
                let mut tampered = tree.clone();
                tampered.peers.get_mut(peer).unwrap().path[step][0] ^= 1;
                assert!(!tampered.verify_peer(peer), "step {step}");
            }
        }
    }

    #[test]
    fn test_flipped_position_fails() {
        let peers = random_peers(9);
        let tree = tree_over(&peers, &Keypair::generate_ed25519());

        for peer in &peers {
            for step in 0..tree.peers[peer].position.len() {
                let mut tampered = tree.clone();
                let position = &mut tampered.peers.get_mut(peer).unwrap().position[step];
                *position = 1 - *position;
                assert!(!tampered.verify_peer(peer), "step {step}");
            }
        }
    }

    #[test]
    fn test_proof_moved_to_another_peer_fails() {
        let peers = random_peers(8);
        let tree = tree_over(&peers, &Keypair::generate_ed25519());

        let mut tampered = tree.clone();
        let proof = tampered.peers[&peers[0]].clone();
        tampered.peers.insert(peers[1], proof);
        assert!(!tampered.verify_peer(&peers[1]));

        // An outsider given a member's proof does not verify either
        let outsider = PeerId::random();
        let mut tampered = tree.clone();
        tampered
            .peers
            .insert(outsider, tree.peers[&peers[0]].clone());
        assert!(!tampered.verify_peer(&outsider));
    }

    #[test]
    fn test_proof_with_lengths_that_differ_fails() {
        let peers = random_peers(5);
        let tree = tree_over(&peers, &Keypair::generate_ed25519());

        for peer in &peers {
            // One position too many: zipping would ignore it
            let mut tampered = tree.clone();
            tampered.peers.get_mut(peer).unwrap().position.push(0);
            assert!(!tampered.verify_peer(peer));

            // One path element too many
            let mut tampered = tree.clone();
            tampered.peers.get_mut(peer).unwrap().path.push([0u8; 32]);
            assert!(!tampered.verify_peer(peer));
        }

        // A single peer, whose valid proof is empty
        let peer = PeerId::random();
        let mut tree = tree_over(&[peer], &Keypair::generate_ed25519());
        tree.peers.get_mut(&peer).unwrap().position.push(1);
        assert!(!tree.verify_peer(&peer));
    }

    #[test]
    fn test_proof_with_a_position_of_two_fails() {
        let peers = random_peers(9);
        let tree = tree_over(&peers, &Keypair::generate_ed25519());

        for peer in &peers {
            for step in 0..tree.peers[peer].position.len() {
                let mut tampered = tree.clone();
                tampered.peers.get_mut(peer).unwrap().position[step] = 2;
                assert!(!tampered.verify_peer(peer), "step {step}");

                let mut tampered = tree.clone();
                tampered.peers.get_mut(peer).unwrap().position[step] = usize::MAX;
                assert!(!tampered.verify_peer(peer), "step {step}");
            }
        }
    }

    #[test]
    fn test_signature_verifies_with_the_relayer_key_only() {
        let relayer = Keypair::generate_ed25519();
        let tree = tree_over(&random_peers(3), &relayer);

        assert!(tree.verify_signature(&relayer.public()).unwrap());
        let other = Keypair::generate_ed25519();
        assert!(!tree.verify_signature(&other.public()).unwrap());
    }

    #[test]
    fn test_signature_fails_once_the_root_or_timestamp_changed() {
        let relayer = Keypair::generate_ed25519();
        let tree = tree_over(&random_peers(3), &relayer);

        let mut tampered = tree.clone();
        tampered.root[31] ^= 1;
        assert!(!tampered.verify_signature(&relayer.public()).unwrap());

        let mut tampered = tree.clone();
        tampered.timestamp += 1;
        assert!(!tampered.verify_signature(&relayer.public()).unwrap());
    }

    #[test]
    fn test_signed_message_is_tag_root_then_big_endian_timestamp() {
        let relayer = Keypair::generate_ed25519();
        let tree = tree_over(&random_peers(2), &relayer);

        let mut message = b"chaincraft/authorized-peers/v1".to_vec();
        message.extend_from_slice(&tree.root);
        message.extend_from_slice(&tree.timestamp.to_be_bytes());
        assert!(relayer.public().verify(&message, &tree.signature));

        // The untagged message the WIP signed is not accepted
        let mut untagged = tree.clone();
        let mut message = tree.root.to_vec();
        message.extend_from_slice(&tree.timestamp.to_be_bytes());
        untagged.signature = relayer.sign(&message).unwrap();
        assert!(!untagged.verify_signature(&relayer.public()).unwrap());
    }

    #[test]
    fn test_known_answer_root_over_the_test_seeds() {
        let peers: Vec<PeerId> = ["relayer_test", "validator_test"]
            .iter()
            .map(|seed| {
                generate_keypair(seed_phrase_to_bytes(Some(seed)))
                    .public()
                    .to_peer_id()
            })
            .collect();
        assert_eq!(
            peers,
            vec![
                PeerId::from_str("12D3KooWAPWpdc7AWArNdQyag5qZKHi1mQSnAdupc2ejJ1ghMhhn").unwrap(),
                PeerId::from_str("12D3KooWDgCeJ15KaPzEEhV8XJd1xoN7hYBBVLdx8tBPiDgA5Lfo").unwrap(),
            ]
        );

        let tree = tree_over(&peers, &Keypair::generate_ed25519());

        assert_eq!(
            blake3::Hash::from(tree.root).to_hex().as_str(),
            KNOWN_ROOT_HEX
        );
    }

    #[test]
    fn test_next_timestamp_is_now_unless_the_previous_is_not_older() {
        assert_eq!(AuthorizedPeersTree::next_timestamp(100, 200), 200);
        assert_eq!(AuthorizedPeersTree::next_timestamp(199, 200), 200);
        assert_eq!(AuthorizedPeersTree::next_timestamp(200, 200), 201);
        assert_eq!(AuthorizedPeersTree::next_timestamp(500, 200), 501);
        assert_eq!(AuthorizedPeersTree::next_timestamp(i64::MAX, 200), i64::MAX);
    }

    #[test]
    fn test_updates_in_the_same_second_get_increasing_timestamps() {
        let relayer = Keypair::generate_ed25519();
        let mut tree = AuthorizedPeersTree::new();
        let mut previous = tree.timestamp;
        for _ in 0..5 {
            tree.update_tree(random_peers(2), &relayer).unwrap();
            assert!(tree.timestamp > previous);
            assert!(tree.verify_signature(&relayer.public()).unwrap());
            previous = tree.timestamp;
        }
    }

    #[test]
    fn test_update_after_a_tree_from_the_future_stays_after_it() {
        let relayer = Keypair::generate_ed25519();
        let mut tree = AuthorizedPeersTree::new();
        tree.timestamp = Utc::now().timestamp() + 3600;
        let future = tree.timestamp;

        tree.update_tree(random_peers(1), &relayer).unwrap();

        assert_eq!(tree.timestamp, future + 1);
    }

    #[test]
    fn test_a_built_tree_is_consistent() {
        let relayer = Keypair::generate_ed25519();
        for size in SIZES {
            assert!(tree_over(&random_peers(size), &relayer).is_consistent());
        }
    }

    #[test]
    fn test_a_tree_with_an_edited_peer_list_is_inconsistent() {
        let relayer = Keypair::generate_ed25519();
        let tree = tree_over(&random_peers(3), &relayer);

        let mut added = tree.clone();
        added.peers.insert(
            PeerId::random(),
            MerkleProof {
                path: Vec::new(),
                position: Vec::new(),
            },
        );
        assert!(!added.is_consistent());

        let mut removed = tree.clone();
        let first = *removed.peers.keys().next().unwrap();
        removed.peers.remove(&first);
        assert!(!removed.is_consistent());

        let mut emptied = tree.clone();
        emptied.peers.clear();
        assert!(!emptied.is_consistent());
    }

    #[test]
    fn test_a_tree_with_an_edited_root_or_proof_is_inconsistent() {
        let relayer = Keypair::generate_ed25519();
        let tree = tree_over(&random_peers(3), &relayer);

        let mut root = tree.clone();
        root.root[0] ^= 1;
        assert!(!root.is_consistent());

        let mut proof = tree.clone();
        let first = *proof.peers.keys().next().unwrap();
        proof.peers.get_mut(&first).unwrap().position[0] ^= 1;
        assert!(!proof.is_consistent());
    }
}
