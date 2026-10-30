//! The relayer's authorized peers: the set, the signed tree over it, and the
//! file that keeps that tree across restarts.

use libp2p::{identity::Keypair, PeerId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions, Permissions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::types::NodeError;
use crate::utils::auth::AuthorizedPeersTree;
use crate::utils::peer_id::UserDirectoryProvider;

/// Where the file sits, under the home directory.
const STORE_PATH: &[&str] = &[".chaincraft", "authorized_peers.json"];
const DIR_PERMISSIONS: u32 = 0o700;
const FILE_PERMISSIONS: u32 = 0o600;

/// The signed tree on disk: the JSON encoding of `AuthorizedPeersTree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeStore {
    path: PathBuf,
}

impl TreeStore {
    /// The file `$HOME/.chaincraft/authorized_peers.json`.
    pub fn in_home<T: UserDirectoryProvider>(provider: &T) -> Result<Self, NodeError> {
        let home = provider
            .get_user_home_dir()
            .ok_or_else(|| NodeError::AuthorizedPeers("Home directory not found".to_string()))?;
        Ok(Self {
            path: STORE_PATH
                .iter()
                .fold(home, |path, component| path.join(component)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn temporary_path(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".tmp");
        PathBuf::from(name)
    }

    /// The stored tree, or `None` when there is no file. A file that cannot
    /// be read or parsed is an error.
    pub fn load(&self) -> Result<Option<AuthorizedPeersTree>, NodeError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.error("Cannot read", e)),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| self.error("Cannot parse", e))
    }

    /// Writes the tree to a temporary file, then renames it over the stored
    /// one, so a crash leaves either the old tree or the new one.
    pub fn save(&self, tree: &AuthorizedPeersTree) -> Result<(), NodeError> {
        let json = serde_json::to_vec_pretty(tree).map_err(|e| self.error("Cannot encode", e))?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| self.error("Cannot create the directory of", e))?;
            fs::set_permissions(dir, Permissions::from_mode(DIR_PERMISSIONS))
                .map_err(|e| self.error("Cannot restrict the directory of", e))?;
        }

        let temporary = self.temporary_path();
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(FILE_PERMISSIONS)
            .open(&temporary)
            .map_err(|e| self.error("Cannot write", e))?;
        // A leftover temporary file keeps its mode: set it again.
        file.set_permissions(Permissions::from_mode(FILE_PERMISSIONS))
            .map_err(|e| self.error("Cannot write", e))?;
        file.write_all(&json)
            .and_then(|_| file.sync_all())
            .map_err(|e| self.error("Cannot write", e))?;
        fs::rename(&temporary, &self.path).map_err(|e| self.error("Cannot replace", e))
    }

    fn error(&self, what: &str, e: impl std::fmt::Display) -> NodeError {
        NodeError::AuthorizedPeers(format!("{} {}: {}", what, self.path.display(), e))
    }
}

/// Where the relayer keeps its tree: nowhere under `--local-test`, which
/// leaves the home directory alone, as it does for the keypair.
pub fn tree_store<T: UserDirectoryProvider>(
    local_test: bool,
    provider: &T,
) -> Result<Option<TreeStore>, NodeError> {
    if local_test {
        Ok(None)
    } else {
        TreeStore::in_home(provider).map(Some)
    }
}

/// What the RPC answers about the authorized peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedNodes {
    /// The authorized peer IDs, sorted as strings.
    pub peer_ids: Vec<String>,
    /// The root of the signed tree, in lowercase hex.
    pub root: String,
    /// The timestamp of the signed tree, in seconds.
    pub timestamp: i64,
}

/// The relayer's authorized peers, always held as a tree signed with the
/// relayer's key and, outside `--local-test`, stored before it is used.
pub struct AuthorizedPeers {
    tree: AuthorizedPeersTree,
    keypair: Keypair,
    store: Option<TreeStore>,
}

impl AuthorizedPeers {
    /// Loads the stored tree. A missing file means no authorized peer: the
    /// empty tree is signed and stored. A file that does not parse, is not
    /// signed by `keypair`, or whose peers do not match its root is an
    /// error: falling back to an empty, newer tree would disconnect every
    /// validator.
    pub fn load(keypair: Keypair, store: Option<TreeStore>) -> Result<Self, NodeError> {
        let stored = match &store {
            Some(store) => store.load()?,
            None => None,
        };
        match stored {
            Some(tree) => {
                Self::check(&tree, &keypair, store.as_ref())?;
                Ok(Self {
                    tree,
                    keypair,
                    store,
                })
            }
            None => {
                let mut authorized = Self {
                    tree: AuthorizedPeersTree::new(),
                    keypair,
                    store,
                };
                authorized.commit(BTreeSet::new())?;
                Ok(authorized)
            }
        }
    }

    fn check(
        tree: &AuthorizedPeersTree,
        keypair: &Keypair,
        store: Option<&TreeStore>,
    ) -> Result<(), NodeError> {
        let file = store.map_or_else(String::new, |store| store.path().display().to_string());
        let signed = tree
            .verify_signature(&keypair.public())
            .map_err(|e| NodeError::AuthorizedPeers(format!("{}: {}", file, e)))?;
        if !signed {
            return Err(NodeError::AuthorizedPeers(format!(
                "{}: not signed by this relayer's key",
                file
            )));
        }
        if !tree.is_consistent() {
            return Err(NodeError::AuthorizedPeers(format!(
                "{}: the peers do not match the signed root",
                file
            )));
        }
        Ok(())
    }

    pub fn tree(&self) -> &AuthorizedPeersTree {
        &self.tree
    }

    pub fn peers(&self) -> impl Iterator<Item = &PeerId> {
        self.tree.peers.keys()
    }

    pub fn contains(&self, peer: &PeerId) -> bool {
        self.tree.peers.contains_key(peer)
    }

    pub fn state(&self) -> AuthorizedNodes {
        let mut peer_ids: Vec<String> = self.peers().map(PeerId::to_string).collect();
        peer_ids.sort();
        AuthorizedNodes {
            peer_ids,
            root: self
                .tree
                .root
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect(),
            timestamp: self.tree.timestamp,
        }
    }

    /// Authorizes `peers` and returns those that were not authorized yet.
    /// When there are none, nothing is signed.
    pub fn authorize(&mut self, peers: &[PeerId]) -> Result<Vec<PeerId>, NodeError> {
        let added: BTreeSet<PeerId> = Self::non_empty(peers)?
            .filter(|peer| !self.contains(peer))
            .collect();
        if !added.is_empty() {
            let mut next: BTreeSet<PeerId> = self.peers().copied().collect();
            next.extend(added.iter().copied());
            self.commit(next)?;
        }
        Ok(added.into_iter().collect())
    }

    /// Revokes `peers` and returns those that were authorized. When there
    /// are none, nothing is signed.
    pub fn revoke(&mut self, peers: &[PeerId]) -> Result<Vec<PeerId>, NodeError> {
        let removed: BTreeSet<PeerId> = Self::non_empty(peers)?
            .filter(|peer| self.contains(peer))
            .collect();
        if !removed.is_empty() {
            let next: BTreeSet<PeerId> = self
                .peers()
                .filter(|peer| !removed.contains(peer))
                .copied()
                .collect();
            self.commit(next)?;
        }
        Ok(removed.into_iter().collect())
    }

    fn non_empty(peers: &[PeerId]) -> Result<impl Iterator<Item = PeerId> + '_, NodeError> {
        if peers.is_empty() {
            return Err(NodeError::AuthorizedPeers("No peer ID given".to_string()));
        }
        Ok(peers.iter().copied())
    }

    // Signs the tree over `peers` and stores it, then makes it current. A
    // tree that cannot be stored is not used.
    fn commit(&mut self, peers: BTreeSet<PeerId>) -> Result<(), NodeError> {
        let mut tree = self.tree.clone();
        tree.update_tree(peers.into_iter().collect(), &self.keypair)
            .map_err(|e| NodeError::AuthorizedPeers(e.to_string()))?;
        if let Some(store) = &self.store {
            store.save(&tree)?;
        }
        self.tree = tree;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::peer_id::{generate_keypair, seed_phrase_to_bytes};
    use tempfile::TempDir;

    // A temporary home: the real one is never touched.
    struct TempHome {
        dir: TempDir,
    }

    impl TempHome {
        fn new() -> Self {
            Self {
                dir: TempDir::new().expect("Failed to create temp directory"),
            }
        }

        fn store(&self) -> TreeStore {
            tree_store(false, self).unwrap().unwrap()
        }

        fn file(&self) -> PathBuf {
            self.dir
                .path()
                .join(".chaincraft")
                .join("authorized_peers.json")
        }
    }

    impl UserDirectoryProvider for TempHome {
        fn get_user_home_dir(&self) -> Option<PathBuf> {
            Some(self.dir.path().to_path_buf())
        }
    }

    struct NoHome;

    impl UserDirectoryProvider for NoHome {
        fn get_user_home_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    fn peer() -> PeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    fn in_memory() -> AuthorizedPeers {
        AuthorizedPeers::load(Keypair::generate_ed25519(), None).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn error_message(result: Result<impl std::fmt::Debug, NodeError>) -> String {
        match result {
            Err(NodeError::AuthorizedPeers(message)) => message,
            other => panic!("expected an AuthorizedPeers error, got {:?}", other),
        }
    }

    fn sorted_strings(peers: &[PeerId]) -> Vec<String> {
        let mut peers: Vec<String> = peers.iter().map(PeerId::to_string).collect();
        peers.sort();
        peers
    }

    #[test]
    fn test_day_one_is_the_empty_tree_signed() {
        let keypair = Keypair::generate_ed25519();
        let authorized = AuthorizedPeers::load(keypair.clone(), None).unwrap();

        let state = authorized.state();
        assert!(state.peer_ids.is_empty());
        assert_eq!(state.root, "00".repeat(32));
        assert!(!authorized.tree().signature.is_empty());
        assert!(authorized
            .tree()
            .verify_signature(&keypair.public())
            .unwrap());
    }

    #[test]
    fn test_authorize_adds_the_new_peers_and_signs_a_newer_tree() {
        let keypair = Keypair::generate_ed25519();
        let mut authorized = AuthorizedPeers::load(keypair.clone(), None).unwrap();
        let before = authorized.state().timestamp;
        let (a, b) = (peer(), peer());

        let mut added = authorized.authorize(&[a, b, a]).unwrap();

        added.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(added, expected);
        let state = authorized.state();
        assert_eq!(state.peer_ids, sorted_strings(&[a, b]));
        assert!(state.timestamp > before);
        let tree = authorized.tree();
        let root: String = tree.root.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(state.root, root);
        assert_ne!(state.root, "00".repeat(32));
        assert!(tree.verify_peer(&a) && tree.verify_peer(&b));
        assert!(tree.is_consistent());
        assert!(tree.verify_signature(&keypair.public()).unwrap());
        assert!(authorized.contains(&a));
    }

    #[test]
    fn test_peer_ids_are_sorted_as_strings() {
        let mut authorized = in_memory();
        let peers: Vec<PeerId> = (0..8).map(|_| peer()).collect();

        authorized.authorize(&peers).unwrap();

        assert_eq!(authorized.state().peer_ids, sorted_strings(&peers));
    }

    #[test]
    fn test_authorize_of_known_peers_changes_nothing_and_signs_nothing() {
        let mut authorized = in_memory();
        let a = peer();
        authorized.authorize(&[a]).unwrap();
        let tree = authorized.tree().clone();

        assert!(authorized.authorize(&[a]).unwrap().is_empty());

        assert_eq!(authorized.tree().timestamp, tree.timestamp);
        assert_eq!(authorized.tree().signature, tree.signature);
        assert_eq!(authorized.state().peer_ids, sorted_strings(&[a]));
    }

    #[test]
    fn test_authorize_returns_only_the_peers_it_added() {
        let mut authorized = in_memory();
        let (a, b) = (peer(), peer());
        authorized.authorize(&[a]).unwrap();

        assert_eq!(authorized.authorize(&[a, b]).unwrap(), vec![b]);
        assert_eq!(authorized.state().peer_ids, sorted_strings(&[a, b]));
    }

    #[test]
    fn test_revoke_removes_the_peers_and_signs_a_newer_tree() {
        let keypair = Keypair::generate_ed25519();
        let mut authorized = AuthorizedPeers::load(keypair.clone(), None).unwrap();
        let (a, b, outsider) = (peer(), peer(), peer());
        authorized.authorize(&[a, b]).unwrap();
        let before = authorized.state().timestamp;

        assert_eq!(authorized.revoke(&[a, outsider]).unwrap(), vec![a]);

        let state = authorized.state();
        assert_eq!(state.peer_ids, sorted_strings(&[b]));
        assert!(state.timestamp > before);
        assert!(!authorized.contains(&a));
        assert!(!authorized.tree().verify_peer(&a));
        assert!(authorized.tree().is_consistent());
        assert!(authorized
            .tree()
            .verify_signature(&keypair.public())
            .unwrap());
    }

    #[test]
    fn test_revoke_of_unknown_peers_changes_nothing_and_signs_nothing() {
        let mut authorized = in_memory();
        authorized.authorize(&[peer()]).unwrap();
        let tree = authorized.tree().clone();

        assert!(authorized.revoke(&[peer()]).unwrap().is_empty());

        assert_eq!(authorized.tree().timestamp, tree.timestamp);
        assert_eq!(authorized.tree().signature, tree.signature);
        assert_eq!(authorized.tree().root, tree.root);
    }

    #[test]
    fn test_an_empty_list_is_an_error_and_changes_nothing() {
        let mut authorized = in_memory();
        let a = peer();
        authorized.authorize(&[a]).unwrap();
        let tree = authorized.tree().clone();

        assert_eq!(error_message(authorized.authorize(&[])), "No peer ID given");
        assert_eq!(error_message(authorized.revoke(&[])), "No peer ID given");

        assert_eq!(authorized.tree().signature, tree.signature);
        assert_eq!(authorized.state().peer_ids, sorted_strings(&[a]));
    }

    #[test]
    fn test_updates_in_the_same_second_get_increasing_timestamps() {
        let mut authorized = in_memory();
        let mut previous = authorized.state().timestamp;
        for _ in 0..4 {
            let a = peer();
            authorized.authorize(&[a]).unwrap();
            assert!(authorized.state().timestamp > previous);
            previous = authorized.state().timestamp;
            authorized.revoke(&[a]).unwrap();
            assert!(authorized.state().timestamp > previous);
            previous = authorized.state().timestamp;
        }
    }

    #[test]
    fn test_the_store_is_under_the_home_directory() {
        let home = TempHome::new();
        assert_eq!(home.store().path(), home.file());
        assert!(TreeStore::in_home(&NoHome).is_err());
    }

    #[test]
    fn test_local_test_has_no_store_and_writes_nothing() {
        let home = TempHome::new();
        assert_eq!(tree_store(true, &home).unwrap(), None);
        // Even without a home directory.
        assert_eq!(tree_store(true, &NoHome).unwrap(), None);

        let mut authorized = AuthorizedPeers::load(
            Keypair::generate_ed25519(),
            tree_store(true, &home).unwrap(),
        )
        .unwrap();
        authorized.authorize(&[peer()]).unwrap();
        authorized.revoke(&[peer()]).unwrap();

        assert_eq!(fs::read_dir(home.dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn test_a_missing_file_is_an_empty_list_whose_tree_is_stored() {
        let home = TempHome::new();
        let keypair = Keypair::generate_ed25519();

        let authorized = AuthorizedPeers::load(keypair, Some(home.store())).unwrap();

        assert!(authorized.state().peer_ids.is_empty());
        let stored = home.store().load().unwrap().unwrap();
        assert_eq!(stored.timestamp, authorized.tree().timestamp);
        assert_eq!(stored.signature, authorized.tree().signature);
    }

    #[test]
    fn test_persistence_round_trip() {
        let home = TempHome::new();
        let keypair = Keypair::generate_ed25519();
        let mut authorized = AuthorizedPeers::load(keypair.clone(), Some(home.store())).unwrap();
        let (a, b) = (peer(), peer());
        authorized.authorize(&[a, b]).unwrap();
        authorized.revoke(&[a]).unwrap();
        let state = authorized.state();

        let reloaded = AuthorizedPeers::load(keypair.clone(), Some(home.store())).unwrap();

        assert_eq!(reloaded.state(), state);
        assert_eq!(reloaded.tree().signature, authorized.tree().signature);
        assert_eq!(mode(&home.file()), 0o600);
        assert_eq!(mode(home.file().parent().unwrap()), 0o700);
        assert!(!home.store().temporary_path().exists());
        // The file is the JSON encoding of the signed tree.
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(home.file()).unwrap()).unwrap();
        assert_eq!(json["timestamp"], state.timestamp);
        assert!(json["peers"].get(b.to_string()).is_some());
    }

    #[test]
    fn test_a_restart_keeps_timestamps_increasing() {
        let home = TempHome::new();
        let keypair = Keypair::generate_ed25519();
        let mut authorized = AuthorizedPeers::load(keypair.clone(), Some(home.store())).unwrap();
        authorized.authorize(&[peer()]).unwrap();
        let before = authorized.state().timestamp;

        let mut restarted = AuthorizedPeers::load(keypair, Some(home.store())).unwrap();
        assert_eq!(restarted.state().timestamp, before);
        restarted.authorize(&[peer()]).unwrap();

        assert!(restarted.state().timestamp > before);
    }

    #[test]
    fn test_save_restricts_a_directory_and_a_leftover_temporary_file() {
        let home = TempHome::new();
        let store = home.store();
        let dir = home.file().parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
        fs::write(store.temporary_path(), b"leftover").unwrap();
        fs::set_permissions(store.temporary_path(), Permissions::from_mode(0o644)).unwrap();

        AuthorizedPeers::load(Keypair::generate_ed25519(), Some(store)).unwrap();

        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&home.file()), 0o600);
    }

    #[test]
    fn test_a_corrupt_file_is_refused_and_left_alone() {
        let home = TempHome::new();
        fs::create_dir_all(home.file().parent().unwrap()).unwrap();
        fs::write(home.file(), b"{ not json").unwrap();

        let message = error_message(
            AuthorizedPeers::load(Keypair::generate_ed25519(), Some(home.store())).map(|_| ()),
        );

        assert!(message.starts_with("Cannot parse "), "{}", message);
        assert_eq!(fs::read(home.file()).unwrap(), b"{ not json");
    }

    #[test]
    fn test_an_unreadable_file_is_refused() {
        let home = TempHome::new();
        // A directory where the file should be: it exists, and cannot be read.
        fs::create_dir_all(home.file()).unwrap();

        let message = error_message(
            AuthorizedPeers::load(Keypair::generate_ed25519(), Some(home.store())).map(|_| ()),
        );

        assert!(message.starts_with("Cannot read "), "{}", message);
    }

    #[test]
    fn test_a_file_signed_by_another_key_is_refused_and_left_alone() {
        let home = TempHome::new();
        let mut other =
            AuthorizedPeers::load(Keypair::generate_ed25519(), Some(home.store())).unwrap();
        other.authorize(&[peer()]).unwrap();
        let written = fs::read(home.file()).unwrap();

        let message = error_message(
            AuthorizedPeers::load(Keypair::generate_ed25519(), Some(home.store())).map(|_| ()),
        );

        assert!(
            message.ends_with("not signed by this relayer's key"),
            "{}",
            message
        );
        assert_eq!(fs::read(home.file()).unwrap(), written);
    }

    #[test]
    fn test_a_file_whose_peers_were_edited_is_refused() {
        let home = TempHome::new();
        let keypair = Keypair::generate_ed25519();
        let mut authorized = AuthorizedPeers::load(keypair.clone(), Some(home.store())).unwrap();
        authorized.authorize(&[peer(), peer()]).unwrap();
        // The signature covers the root and the timestamp, not the list.
        let mut edited = authorized.tree().clone();
        let first = *edited.peers.keys().next().unwrap();
        edited.peers.remove(&first);
        home.store().save(&edited).unwrap();

        let message = error_message(AuthorizedPeers::load(keypair, Some(home.store())).map(|_| ()));

        assert!(
            message.ends_with("the peers do not match the signed root"),
            "{}",
            message
        );
    }

    #[test]
    fn test_a_tree_that_cannot_be_stored_is_not_used() {
        let home = TempHome::new();
        let mut authorized =
            AuthorizedPeers::load(Keypair::generate_ed25519(), Some(home.store())).unwrap();
        let state = authorized.state();
        // A file where the directory should be: nothing can be written.
        let dir = home.file().parent().unwrap().to_path_buf();
        fs::remove_dir_all(&dir).unwrap();
        fs::write(&dir, b"").unwrap();

        assert!(authorized.authorize(&[peer()]).is_err());

        assert_eq!(authorized.state(), state);
    }

    // The tree of the fixture: the peers of the seeds `relayer_test` and
    // `validator_test`, signed with the key of `relayer_test` at a fixed
    // time. Ed25519 signatures are deterministic, so this tree always
    // encodes to the same bytes.
    const FIXTURE_TIMESTAMP: i64 = 1_767_225_600;

    /// The file the relayer writes for the fixture tree.
    const AUTHORIZED_PEERS_FILE: &str = include_str!("testdata/authorized_peers.json");

    fn seed_keypair(seed: &str) -> Keypair {
        generate_keypair(seed_phrase_to_bytes(Some(seed)))
    }

    fn fixture_tree() -> AuthorizedPeersTree {
        let relayer = seed_keypair("relayer_test");
        let peers = ["relayer_test", "validator_test"]
            .iter()
            .map(|seed| seed_keypair(seed).public().to_peer_id())
            .collect();
        let mut tree = AuthorizedPeersTree::new();
        tree.update_tree(peers, &relayer).unwrap();
        tree.timestamp = FIXTURE_TIMESTAMP;
        let mut message = b"chaincraft/authorized-peers/v1".to_vec();
        message.extend_from_slice(&tree.root);
        message.extend_from_slice(&FIXTURE_TIMESTAMP.to_be_bytes());
        tree.signature = relayer.sign(&message).unwrap();
        tree
    }

    #[test]
    fn test_authorized_peers_file_format_is_stable() {
        let home = TempHome::new();

        home.store().save(&fixture_tree()).unwrap();

        assert_eq!(
            fs::read_to_string(home.file()).unwrap(),
            AUTHORIZED_PEERS_FILE
        );
    }

    #[test]
    fn test_existing_authorized_peers_file_is_read_back() {
        let home = TempHome::new();
        fs::create_dir_all(home.file().parent().unwrap()).unwrap();
        fs::write(home.file(), AUTHORIZED_PEERS_FILE).unwrap();

        let authorized =
            AuthorizedPeers::load(seed_keypair("relayer_test"), Some(home.store())).unwrap();

        assert_eq!(
            authorized.state(),
            AuthorizedNodes {
                peer_ids: vec![
                    "12D3KooWAPWpdc7AWArNdQyag5qZKHi1mQSnAdupc2ejJ1ghMhhn".to_string(),
                    "12D3KooWDgCeJ15KaPzEEhV8XJd1xoN7hYBBVLdx8tBPiDgA5Lfo".to_string(),
                ],
                root: "4c7899a581576f2349c4c551cb5e079298e353b7c649c9694df194d75bcf9d17"
                    .to_string(),
                timestamp: FIXTURE_TIMESTAMP,
            }
        );
    }
}
