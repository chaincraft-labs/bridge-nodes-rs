//! A validator's view of the allowlist: the relayer's key, the last signed
//! tree it accepted, and the peers that makes it allow.

use libp2p::identity::PublicKey;
use libp2p::PeerId;
use std::collections::BTreeSet;
use std::fmt;

use crate::types::NodeError;
use crate::utils::auth::{AuthorizedPeersTree, WireTree};

/// The multihash code of a peer ID that embeds its public key.
const IDENTITY_MULTIHASH: u64 = 0x00;

/// The public key a peer ID embeds, or `None` when it embeds none (a hash
/// of a larger key) or when the key it embeds does not yield that same ID.
pub fn embedded_public_key(peer: &PeerId) -> Option<PublicKey> {
    let multihash = peer.as_ref();
    if multihash.code() != IDENTITY_MULTIHASH {
        return None;
    }
    PublicKey::try_decode_protobuf(multihash.digest())
        .ok()
        .filter(|key| key.to_peer_id() == *peer)
}

/// Why a received tree was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The bytes do not decode to a tree.
    Malformed(String),
    /// The relayer's key did not sign it.
    NotSigned,
    /// Its peers are not those of its signed root.
    Inconsistent,
    /// It is not newer than the tree held.
    NotNewer { held: i64, received: i64 },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Malformed(e) => write!(f, "malformed: {}", e),
            Refusal::NotSigned => write!(f, "not signed by the relayer's key"),
            Refusal::Inconsistent => write!(f, "its peers do not match its signed root"),
            Refusal::NotNewer { held, received } => write!(
                f,
                "its timestamp {} is not newer than the held tree's {}",
                received, held
            ),
        }
    }
}

/// What accepting a tree changed in the allowed peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub added: Vec<PeerId>,
    pub removed: Vec<PeerId>,
    /// The peer count, root and timestamp of the accepted tree.
    pub peers: usize,
    pub root: [u8; 32],
    pub timestamp: i64,
}

/// The tree a validator holds, and the relayer whose key must sign it.
#[derive(Debug, Clone)]
pub struct HeldTree {
    relayer: PeerId,
    relayer_key: PublicKey,
    tree: Option<AuthorizedPeersTree>,
}

impl HeldTree {
    /// No tree yet, for the relayer `relayer`, whose ID must embed its key.
    pub fn new(relayer: PeerId) -> Result<Self, NodeError> {
        let relayer_key = embedded_public_key(&relayer).ok_or_else(|| {
            NodeError::ConfigError(format!(
                "The bootstrap peer ID {} embeds no public key: the relayer's signature cannot be checked",
                relayer
            ))
        })?;
        Ok(Self {
            relayer,
            relayer_key,
            tree: None,
        })
    }

    pub fn relayer(&self) -> PeerId {
        self.relayer
    }

    pub fn tree(&self) -> Option<&AuthorizedPeersTree> {
        self.tree.as_ref()
    }

    /// The peers a validator lets connect: the relayer, and the peers of the
    /// tree it holds.
    pub fn allowed(&self) -> BTreeSet<PeerId> {
        let mut allowed: BTreeSet<PeerId> = self
            .tree
            .iter()
            .flat_map(|tree| tree.peers.keys().copied())
            .collect();
        allowed.insert(self.relayer);
        allowed
    }

    /// Accepts `tree` when the relayer signed it, its peers are those of its
    /// root, and it is newer than the tree held. A refused tree leaves the
    /// held one as it is.
    pub fn accept(&mut self, tree: AuthorizedPeersTree) -> Result<Accepted, Refusal> {
        // The signature first: nothing else of an unsigned tree is trusted.
        if !tree
            .verify_signature(&self.relayer_key)
            .map_err(|e| Refusal::Malformed(e.to_string()))?
        {
            return Err(Refusal::NotSigned);
        }
        if !tree.is_consistent() {
            return Err(Refusal::Inconsistent);
        }
        if let Some(held) = &self.tree {
            if tree.timestamp <= held.timestamp {
                return Err(Refusal::NotNewer {
                    held: held.timestamp,
                    received: tree.timestamp,
                });
            }
        }

        let (peers, root, timestamp) = (tree.peers.len(), tree.root, tree.timestamp);
        let before = self.allowed();
        self.tree = Some(tree);
        let after = self.allowed();
        Ok(Accepted {
            added: after.difference(&before).copied().collect(),
            removed: before.difference(&after).copied().collect(),
            peers,
            root,
            timestamp,
        })
    }

    /// Accepts a tree received in its wire encoding.
    pub fn accept_wire(&mut self, wire: WireTree) -> Result<Accepted, Refusal> {
        let tree = wire.into_tree().map_err(Refusal::Malformed)?;
        self.accept(tree)
    }

    /// Accepts a tree received as bytes, as gossipsub delivers it.
    pub fn accept_bytes(&mut self, bytes: &[u8]) -> Result<Accepted, Refusal> {
        let wire = WireTree::decode(bytes).map_err(Refusal::Malformed)?;
        self.accept_wire(wire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::auth::MerkleProof;
    use libp2p::identity::Keypair;
    use libp2p::multihash::Multihash;

    fn peer() -> PeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    fn sorted(peers: &[PeerId]) -> Vec<PeerId> {
        let mut peers = peers.to_vec();
        peers.sort();
        peers.dedup();
        peers
    }

    /// A relayer signing trees with strictly increasing timestamps.
    struct Relayer {
        keypair: Keypair,
        tree: AuthorizedPeersTree,
    }

    impl Relayer {
        fn new() -> Self {
            Self {
                keypair: Keypair::generate_ed25519(),
                tree: AuthorizedPeersTree::new(),
            }
        }

        fn id(&self) -> PeerId {
            self.keypair.public().to_peer_id()
        }

        fn sign(&mut self, peers: &[PeerId]) -> AuthorizedPeersTree {
            self.tree
                .update_tree(peers.to_vec(), &self.keypair)
                .unwrap();
            self.tree.clone()
        }

        fn held(&self) -> HeldTree {
            HeldTree::new(self.id()).unwrap()
        }
    }

    fn bytes(tree: &AuthorizedPeersTree) -> Vec<u8> {
        WireTree::from(tree).encode().unwrap()
    }

    fn resign(tree: &mut AuthorizedPeersTree, keypair: &Keypair) {
        let mut message = b"chaincraft/authorized-peers/v1".to_vec();
        message.extend_from_slice(&tree.root);
        message.extend_from_slice(&tree.timestamp.to_be_bytes());
        tree.signature = keypair.sign(&message).unwrap();
    }

    #[test]
    fn test_an_ed25519_peer_id_embeds_its_key() {
        let keypair = Keypair::generate_ed25519();
        let peer = keypair.public().to_peer_id();

        assert_eq!(embedded_public_key(&peer), Some(keypair.public()));
        assert!(HeldTree::new(peer).is_ok());
    }

    #[test]
    fn test_a_peer_id_that_embeds_no_key_is_a_configuration_error() {
        // A hashed peer ID, as an RSA key yields.
        let hashed = PeerId::from_multihash(Multihash::wrap(0x12, &[7u8; 32]).unwrap()).unwrap();
        // An identity multihash whose bytes are no key.
        let random = PeerId::random();
        // A key encoded with its fields swapped: it decodes, to a key whose
        // own ID is another one.
        let key = Keypair::generate_ed25519().public();
        let canonical = key.encode_protobuf();
        let mut swapped = canonical[2..].to_vec();
        swapped.extend_from_slice(&canonical[..2]);
        assert_eq!(PublicKey::try_decode_protobuf(&swapped).unwrap(), key);
        let non_canonical =
            PeerId::from_multihash(Multihash::wrap(0x00, &swapped).unwrap()).unwrap();
        assert_ne!(non_canonical, key.to_peer_id());

        for peer in [hashed, random, non_canonical] {
            assert_eq!(embedded_public_key(&peer), None, "{}", peer);
            match HeldTree::new(peer) {
                Err(NodeError::ConfigError(message)) => {
                    assert!(message.contains("embeds no public key"), "{}", message)
                }
                other => panic!("expected a ConfigError, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_a_valid_tree_is_accepted() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let (a, b) = (peer(), peer());
        let tree = relayer.sign(&[a, b]);

        let accepted = held.accept_bytes(&bytes(&tree)).unwrap();

        assert_eq!(accepted.added, sorted(&[a, b]));
        assert!(accepted.removed.is_empty());
        let kept = held.tree().unwrap();
        assert_eq!(kept.root, tree.root);
        assert_eq!(kept.timestamp, tree.timestamp);
        assert_eq!(
            (accepted.peers, accepted.root, accepted.timestamp),
            (2, tree.root, tree.timestamp)
        );
    }

    #[test]
    fn test_the_empty_tree_is_accepted_and_allows_the_relayer_only() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();

        let accepted = held.accept_bytes(&bytes(&relayer.sign(&[]))).unwrap();

        assert!(accepted.added.is_empty() && accepted.removed.is_empty());
        assert!(held.tree().is_some());
        assert_eq!(
            held.allowed().into_iter().collect::<Vec<_>>(),
            vec![relayer.id()]
        );
    }

    #[test]
    fn test_a_tree_signed_by_another_key_is_refused() {
        let relayer = Relayer::new();
        let mut held = relayer.held();
        let mut impostor = Relayer::new();

        let refusal = held.accept_bytes(&bytes(&impostor.sign(&[peer()])));

        assert_eq!(refusal, Err(Refusal::NotSigned));
        assert!(held.tree().is_none());
    }

    #[test]
    fn test_an_equal_or_older_timestamp_is_refused() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let older = relayer.sign(&[peer()]);
        let newer = relayer.sign(&[peer()]);
        held.accept(newer.clone()).unwrap();

        for tree in [newer.clone(), older.clone()] {
            assert_eq!(
                held.accept_bytes(&bytes(&tree)),
                Err(Refusal::NotNewer {
                    held: newer.timestamp,
                    received: tree.timestamp
                })
            );
            assert_eq!(held.tree().unwrap().root, newer.root);
        }
    }

    #[test]
    fn test_a_tree_one_second_newer_is_accepted() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let first = relayer.sign(&[peer()]);
        held.accept(first.clone()).unwrap();
        let mut next = first.clone();
        next.timestamp = first.timestamp + 1;
        resign(&mut next, &relayer.keypair);

        assert!(held.accept(next).is_ok());
        assert_eq!(held.tree().unwrap().timestamp, first.timestamp + 1);
    }

    #[test]
    fn test_a_root_or_timestamp_changed_after_signing_is_refused() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let tree = relayer.sign(&[peer(), peer()]);

        let mut root = tree.clone();
        root.root[0] ^= 1;
        let mut timestamp = tree.clone();
        timestamp.timestamp += 1;
        for tampered in [root, timestamp] {
            assert_eq!(
                held.accept_bytes(&bytes(&tampered)),
                Err(Refusal::NotSigned)
            );
            assert!(held.tree().is_none());
        }
    }

    #[test]
    fn test_an_entry_added_with_a_copied_or_forged_proof_is_refused() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let (a, b, c) = (peer(), peer(), peer());
        let tree = relayer.sign(&[a, b, c]);
        let intruder = peer();

        let mut copied = tree.clone();
        copied.peers.insert(intruder, tree.peers[&a].clone());
        let mut forged = tree.clone();
        forged.peers.insert(
            intruder,
            MerkleProof {
                path: vec![tree.root],
                position: vec![0],
            },
        );
        for tampered in [copied, forged] {
            // The signature still holds: it covers the root only.
            assert!(tampered
                .verify_signature(&relayer.keypair.public())
                .unwrap());
            assert_eq!(held.accept(tampered), Err(Refusal::Inconsistent));
            assert!(held.tree().is_none());
        }

        // On the wire, where proofs do not travel: the intruder in the list.
        let mut wire = WireTree::from(&tree);
        wire.peers.push(intruder);
        assert_eq!(held.accept_wire(wire), Err(Refusal::Inconsistent));
        assert!(held.tree().is_none());
    }

    #[test]
    fn test_an_entry_removed_from_a_signed_tree_is_refused() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let (a, b, c) = (peer(), peer(), peer());
        let tree = relayer.sign(&[a, b, c]);

        let mut removed = tree.clone();
        removed.peers.remove(&b);
        assert_eq!(held.accept(removed), Err(Refusal::Inconsistent));

        let mut wire = WireTree::from(&tree);
        wire.peers.retain(|peer| *peer != b);
        assert_eq!(held.accept_wire(wire), Err(Refusal::Inconsistent));
        assert!(held.tree().is_none());
    }

    #[test]
    fn test_a_refused_tree_leaves_the_held_one() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let a = peer();
        held.accept(relayer.sign(&[a])).unwrap();
        let allowed = held.allowed();

        let mut tampered = relayer.sign(&[a, peer()]);
        tampered.peers.remove(&a);
        assert_eq!(held.accept(tampered), Err(Refusal::Inconsistent));

        assert_eq!(held.allowed(), allowed);
    }

    #[test]
    fn test_malformed_bytes_are_refused_without_a_panic() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let good = bytes(&relayer.sign(&[peer()]));
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"\xff\xfe".to_vec(),
            b"null".to_vec(),
            b"{\"peers\":[]}".to_vec(),
            vec![b'['; 10_000],
            vec![b' '; 70_000],
        ];
        for end in [1, good.len() / 2, good.len() - 1] {
            cases.push(good[..end].to_vec());
        }
        let mut wire: serde_json::Value = serde_json::from_slice(&good).unwrap();
        wire["root"] = "00".repeat(16).into();
        cases.push(serde_json::to_vec(&wire).unwrap());
        wire["root"] = serde_json::Value::from(hex::encode([0u8; 32]));
        wire["signature"] = "abc".into();
        cases.push(serde_json::to_vec(&wire).unwrap());

        for case in cases {
            assert!(
                matches!(held.accept_bytes(&case), Err(Refusal::Malformed(_))),
                "{:?}",
                String::from_utf8_lossy(&case[..case.len().min(40)])
            );
        }
        assert!(held.tree().is_none());

        // A signature of the wrong length decodes, and is not the relayer's.
        let mut short: serde_json::Value = serde_json::from_slice(&good).unwrap();
        short["signature"] = "00".into();
        assert_eq!(
            held.accept_bytes(&serde_json::to_vec(&short).unwrap()),
            Err(Refusal::NotSigned)
        );
    }

    #[test]
    fn test_the_allowed_peers_follow_the_held_tree() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let (a, b, c) = (peer(), peer(), peer());

        // Before any tree: the relayer only.
        assert_eq!(held.allowed(), BTreeSet::from([relayer.id()]));

        // A first tree.
        let accepted = held.accept(relayer.sign(&[a, b])).unwrap();
        assert_eq!(accepted.added, sorted(&[a, b]));
        assert!(accepted.removed.is_empty());
        assert_eq!(held.allowed(), BTreeSet::from([relayer.id(), a, b]));

        // An addition.
        let accepted = held.accept(relayer.sign(&[a, b, c])).unwrap();
        assert_eq!(accepted.added, vec![c]);
        assert!(accepted.removed.is_empty());
        assert_eq!(held.allowed(), BTreeSet::from([relayer.id(), a, b, c]));

        // A revocation.
        let accepted = held.accept(relayer.sign(&[a, c])).unwrap();
        assert!(accepted.added.is_empty());
        assert_eq!(accepted.removed, vec![b]);
        assert_eq!(held.allowed(), BTreeSet::from([relayer.id(), a, c]));
    }

    #[test]
    fn test_the_relayer_stays_allowed_whatever_the_tree() {
        let mut relayer = Relayer::new();
        let mut held = relayer.held();
        let a = peer();
        let id = relayer.id();

        let accepted = held.accept(relayer.sign(&[id, a])).unwrap();
        assert_eq!(accepted.added, vec![a]);

        let accepted = held.accept(relayer.sign(&[a])).unwrap();
        assert!(accepted.removed.is_empty());
        assert!(held.allowed().contains(&id));
        assert_eq!(held.relayer(), id);
    }

    #[test]
    fn test_refusals_say_why() {
        assert_eq!(
            Refusal::Malformed("eof".to_string()).to_string(),
            "malformed: eof"
        );
        assert_eq!(
            Refusal::NotSigned.to_string(),
            "not signed by the relayer's key"
        );
        assert_eq!(
            Refusal::Inconsistent.to_string(),
            "its peers do not match its signed root"
        );
        assert_eq!(
            Refusal::NotNewer {
                held: 5,
                received: 4
            }
            .to_string(),
            "its timestamp 4 is not newer than the held tree's 5"
        );
    }
}
