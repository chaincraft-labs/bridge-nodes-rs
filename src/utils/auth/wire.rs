//! The signed tree as it travels between nodes: published on the gossipsub
//! topic `authorized_peers`, and answered on `/authorized-peers/1.0.0`.
//!
//! The JSON object `{"peers":[…],"root":"…","timestamp":…,"signature":"…"}`:
//! the authorized peer IDs in base58, sorted as `PeerId`s, the root and the
//! signature in lowercase hex, and the timestamp in seconds. The proofs do not
//! travel: the receiver rebuilds them from the peers, then checks that the
//! root it was given is theirs (`is_consistent`).
//!
//! This is not the encoding of `authorized_peers.json`, which is the serde
//! encoding of `AuthorizedPeersTree`: a `[u8; 32]` there is an array of
//! numbers, and every peer carries its proof, which does not fit a gossipsub
//! message for 100 peers.

use libp2p::PeerId;
use serde::{Deserialize, Serialize};

use super::types::AuthorizedPeersTree;

/// The largest encoded tree a node sends or accepts. Gossipsub refuses an
/// RPC frame over 65536 bytes, its default, and the frame carries the
/// message's topic, source, sequence number and signature besides the data.
pub const MAX_WIRE_TREE_BYTES: usize = 65536 - 1024;

/// Whether an encoded tree of `len` bytes may travel.
pub fn fits(len: usize) -> bool {
    len <= MAX_WIRE_TREE_BYTES
}

/// The wire encoding of a signed tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireTree {
    pub peers: Vec<PeerId>,
    pub root: String,
    pub timestamp: i64,
    pub signature: String,
}

/// A validator's request for the relayer's current tree. It carries nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedPeersRequest {}

impl From<&AuthorizedPeersTree> for WireTree {
    fn from(tree: &AuthorizedPeersTree) -> Self {
        Self {
            peers: tree.peers.keys().copied().collect(),
            root: hex::encode(tree.root),
            timestamp: tree.timestamp,
            signature: hex::encode(&tree.signature),
        }
    }
}

impl WireTree {
    /// The bytes published on the topic.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| e.to_string())
    }

    /// Decodes bytes received from the network. Anything malformed, or
    /// larger than a node sends, is an error.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if !fits(bytes.len()) {
            return Err(format!(
                "{} bytes, more than the {} a tree may take",
                bytes.len(),
                MAX_WIRE_TREE_BYTES
            ));
        }
        serde_json::from_slice(bytes).map_err(|e| e.to_string())
    }

    /// The tree it carries, its proofs rebuilt. It is neither checked for
    /// its signature nor for its consistency: the receiver does both.
    pub fn into_tree(self) -> Result<AuthorizedPeersTree, String> {
        let mut root = [0u8; 32];
        hex::decode_to_slice(&self.root, &mut root).map_err(|e| format!("root: {}", e))?;
        let signature = hex::decode(&self.signature).map_err(|e| format!("signature: {}", e))?;
        Ok(AuthorizedPeersTree::from_signed_parts(
            self.peers,
            root,
            self.timestamp,
            signature,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::peer_id::{generate_keypair, seed_phrase_to_bytes};
    use libp2p::identity::Keypair;

    fn seed_keypair(seed: &str) -> Keypair {
        generate_keypair(seed_phrase_to_bytes(Some(seed)))
    }

    fn peers(count: usize) -> Vec<PeerId> {
        (0..count)
            .map(|_| Keypair::generate_ed25519().public().to_peer_id())
            .collect()
    }

    fn signed_tree(peers: &[PeerId], relayer: &Keypair) -> AuthorizedPeersTree {
        let mut tree = AuthorizedPeersTree::new();
        tree.update_tree(peers.to_vec(), relayer).unwrap();
        tree
    }

    fn assert_same_tree(left: &AuthorizedPeersTree, right: &AuthorizedPeersTree) {
        assert_eq!(left.root, right.root);
        assert_eq!(left.peers, right.peers);
        assert_eq!(left.timestamp, right.timestamp);
        assert_eq!(left.signature, right.signature);
    }

    // The tree of the seeds `relayer_test` and `validator_test`, signed with
    // the key of `relayer_test` at a fixed time, as the file's fixture.
    const FIXTURE_TIMESTAMP: i64 = 1_767_225_600;
    const FIXTURE_WIRE: &str = include_str!("testdata/authorized_peers.wire.json");

    fn fixture_tree() -> AuthorizedPeersTree {
        let relayer = seed_keypair("relayer_test");
        let peers: Vec<PeerId> = ["relayer_test", "validator_test"]
            .iter()
            .map(|seed| seed_keypair(seed).public().to_peer_id())
            .collect();
        let mut tree = signed_tree(&peers, &relayer);
        tree.timestamp = FIXTURE_TIMESTAMP;
        let mut message = b"chaincraft/authorized-peers/v1".to_vec();
        message.extend_from_slice(&tree.root);
        message.extend_from_slice(&FIXTURE_TIMESTAMP.to_be_bytes());
        tree.signature = relayer.sign(&message).unwrap();
        tree
    }

    #[test]
    fn test_wire_encoding_is_stable() {
        let encoded = WireTree::from(&fixture_tree()).encode().unwrap();

        assert_eq!(String::from_utf8(encoded).unwrap(), FIXTURE_WIRE.trim_end());
    }

    #[test]
    fn test_the_pinned_wire_tree_decodes_to_the_fixture() {
        let tree = WireTree::decode(FIXTURE_WIRE.as_bytes())
            .unwrap()
            .into_tree()
            .unwrap();

        assert_same_tree(&tree, &fixture_tree());
        assert!(tree.is_consistent());
        assert!(tree
            .verify_signature(&seed_keypair("relayer_test").public())
            .unwrap());
    }

    #[test]
    fn test_a_tree_of_100_peers_fits_and_decodes_back_to_the_same_tree() {
        let relayer = Keypair::generate_ed25519();
        let tree = signed_tree(&peers(100), &relayer);

        let encoded = WireTree::from(&tree).encode().unwrap();

        assert!(encoded.len() <= 65536, "{} bytes", encoded.len());
        assert!(fits(encoded.len()));
        let decoded = WireTree::decode(&encoded).unwrap().into_tree().unwrap();
        assert_same_tree(&decoded, &tree);
        assert!(decoded.is_consistent());
        assert!(decoded.verify_signature(&relayer.public()).unwrap());
        // The file's encoding of the same tree does not fit.
        assert!(serde_json::to_vec(&tree).unwrap().len() > 65536);
    }

    #[test]
    fn test_the_empty_tree_travels() {
        let relayer = Keypair::generate_ed25519();
        let tree = signed_tree(&[], &relayer);

        let encoded = WireTree::from(&tree).encode().unwrap();
        let decoded = WireTree::decode(&encoded).unwrap().into_tree().unwrap();

        assert_same_tree(&decoded, &tree);
        assert_eq!(decoded.root, [0u8; 32]);
        assert!(decoded.is_consistent());
    }

    #[test]
    fn test_the_limit_leaves_room_for_the_gossipsub_envelope() {
        assert!(fits(MAX_WIRE_TREE_BYTES));
        assert!(!fits(MAX_WIRE_TREE_BYTES + 1));
    }

    #[test]
    fn test_bytes_over_the_limit_are_refused_unparsed() {
        // Valid JSON, padded with spaces past the limit.
        let mut padded = vec![b' '; MAX_WIRE_TREE_BYTES];
        padded.extend_from_slice(FIXTURE_WIRE.trim_end().as_bytes());
        assert!(WireTree::decode(&padded).is_err());

        // At the limit exactly, it decodes.
        let fixture = FIXTURE_WIRE.trim_end().as_bytes();
        let mut padded = vec![b' '; MAX_WIRE_TREE_BYTES - fixture.len()];
        padded.extend_from_slice(fixture);
        assert!(WireTree::decode(&padded).is_ok());
    }

    #[test]
    fn test_malformed_bytes_are_errors() {
        let wire = WireTree::from(&fixture_tree());
        let json = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut value = serde_json::to_value(&wire).unwrap();
            edit(&mut value);
            serde_json::to_vec(&value).unwrap()
        };
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("not JSON", b"\xff\x00{".to_vec()),
            ("an array", b"[]".to_vec()),
            (
                "no root",
                json(&|v| drop(v.as_object_mut().unwrap().remove("root"))),
            ),
            ("unknown field", json(&|v| v["extra"] = 1.into())),
            ("bad peer", json(&|v| v["peers"][0] = "not-a-peer".into())),
            ("float timestamp", json(&|v| v["timestamp"] = 1.5.into())),
            (
                "huge timestamp",
                json(&|v| v["timestamp"] = u64::MAX.into()),
            ),
        ];
        for (name, bytes) in cases {
            assert!(WireTree::decode(&bytes).is_err(), "{}", name);
        }

        let into_tree = |edit: &dyn Fn(&mut WireTree)| {
            let mut wire = wire.clone();
            edit(&mut wire);
            wire.into_tree()
        };
        assert!(into_tree(&|w| w.root = "00".repeat(31)).is_err());
        assert!(into_tree(&|w| w.root = "00".repeat(33)).is_err());
        assert!(into_tree(&|w| w.root = "zz".repeat(32)).is_err());
        assert!(into_tree(&|w| w.signature = "0".to_string()).is_err());
        assert!(into_tree(&|w| w.signature = "xy".to_string()).is_err());
    }

    #[test]
    fn test_peers_are_rebuilt_in_order_without_duplicates() {
        let relayer = Keypair::generate_ed25519();
        let tree = signed_tree(&peers(5), &relayer);
        let mut wire = WireTree::from(&tree);
        wire.peers.reverse();
        wire.peers.push(wire.peers[0]);

        let decoded = wire.into_tree().unwrap();

        assert_same_tree(&decoded, &tree);
        assert!(decoded.is_consistent());
    }

    #[test]
    fn test_the_request_is_an_empty_object() {
        assert_eq!(
            serde_json::to_string(&AuthorizedPeersRequest {}).unwrap(),
            "{}"
        );
        assert!(serde_json::from_str::<AuthorizedPeersRequest>("{\"a\":1}").is_err());
    }
}
