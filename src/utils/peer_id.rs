use std::error::Error;
use std::fs::{create_dir_all, set_permissions, File, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use base64::{engine::general_purpose, Engine};
use directories::UserDirs;
use general_purpose::STANDARD;
use libp2p::identity::Keypair;
use libp2p::PeerId;
use sha3::{Digest, Sha3_256};

static DEFAULT_PATH: &[&str] = &[".chaincraft", "keypair.key"];
const PATH_PERMISSIONS: u32 = 0o700;
const FILE_PERMISSIONS: u32 = 0o600;


pub trait UserDirectoryProvider {
    fn get_user_home_dir(&self) -> Option<PathBuf>;
}

pub struct DefaultUserDirectoryProvider;

impl UserDirectoryProvider for DefaultUserDirectoryProvider {
    fn get_user_home_dir(&self) -> Option<PathBuf> {
        UserDirs::new().map(|user_dirs| user_dirs.home_dir().to_path_buf())
    }
}

fn seed_phrase_to_bytes(seed_phrase: Option<&str>) -> Option<[u8; 32]> {
    let seed = seed_phrase?;
    let mut hasher = Sha3_256::new();
    hasher.update(seed.as_bytes());
    let result = hasher.finalize();

    result.as_slice().try_into().ok()
}

fn generate_keypair(secret_key_seed: Option<[u8; 32]>) -> Keypair {
    match secret_key_seed {
        Some(seed) => Keypair::ed25519_from_bytes(seed).unwrap(),
        None => Keypair::generate_ed25519(),
    }
}

fn save_keypair<T: UserDirectoryProvider>(keypair: &Keypair, provider: &T) -> Result<(), Box<dyn Error>> {
    // Encode as protobuf structure.
    let encoded_keypair_pbuf = keypair.to_protobuf_encoding()?;

    // Encode as base64
    let encoded_keypair_pbuf_base64 = general_purpose::STANDARD.encode(&encoded_keypair_pbuf);

    // Save encoded keypair to file
    if let Some(home_dir) = provider.get_user_home_dir() {
        let file_path = DEFAULT_PATH
            .iter()
            .fold(home_dir.to_path_buf(), |path, component| {
                path.join(component)
            });

        if let Some(parent_dir) = file_path.parent() {
            create_dir_all(parent_dir)?;
            set_permissions(parent_dir, Permissions::from_mode(PATH_PERMISSIONS))?;
        }

        let mut file = File::create(&file_path)?;
        file.write_all(encoded_keypair_pbuf_base64.as_bytes())?;
        set_permissions(file_path, Permissions::from_mode(FILE_PERMISSIONS))?;

        Ok(())
    } else {
        Err("Home directory not found".into())
    }
}

pub fn generate_peer_id<T: UserDirectoryProvider>(provider: &T) -> Result<PeerId, Box<dyn Error>> {
    if let Some(home_dir) = provider.get_user_home_dir() {
        let file_path = DEFAULT_PATH
            .iter()
            .fold(home_dir.to_path_buf(), |path, component| {
                path.join(component)
            });
        let mut file = File::open(file_path)?;

        let mut encoded_secret_base64 = String::new();
        file.read_to_string(&mut encoded_secret_base64)?;

        let encoded_secret = STANDARD.decode(&encoded_secret_base64)?;
        let keypair = Keypair::from_protobuf_encoding(&encoded_secret)?;

        Ok(PeerId::from(keypair.public()))
    } else {
        Err("Home directory not found".into())
    }
}

pub fn generate_new_keypair_and_peer_id<T: UserDirectoryProvider>(
    seed_phrase: Option<&str>,
    provider: &T,
) -> Result<PeerId, Box<dyn Error>> {
    let secret_key_seed = seed_phrase_to_bytes(seed_phrase);
    let keypair = generate_keypair(secret_key_seed);
    save_keypair(&keypair, provider)?;
    generate_peer_id(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    struct MockUserDirectoryProvider {
        temp_dir: TempDir,
    }

    impl MockUserDirectoryProvider {
        fn new() -> Self {
            MockUserDirectoryProvider {
                temp_dir: TempDir::new().expect("Failed to create temp directory"),
            }
        }

        fn get_temp_dir_path(&self) -> PathBuf {
            self.temp_dir.path().to_path_buf()
        }
    }

    impl UserDirectoryProvider for MockUserDirectoryProvider {
        fn get_user_home_dir(&self) -> Option<PathBuf> {
            Some(self.temp_dir.path().to_path_buf())
        }
    }

    #[test]
    fn test_get_user_home_dir() {
        let provider = DefaultUserDirectoryProvider;
        let home_dir = provider.get_user_home_dir();

        match home_dir {
            Some(path) => {
                assert!(path.exists(), "Directory does not exist.");
                assert!(path.is_dir(), "Path is not a directory.");
                println!("Home directory detected : {:?}", path);
            }
            None => panic!("Unable to detect home directory."),
        }
    }

    #[test]
    fn test_seed_phrase_to_bytes_with_seed_phrase() {
        // Seed phrase example
        let seed_phrase = Some("test_seed_phrase");

        // Expected output: Some array of 32 bytes
        let result = seed_phrase_to_bytes(seed_phrase);

        match result {
            Some(bytes) => {
                // Check if the output has 32 bytes
                assert_eq!(bytes.len(), 32);
                println!("Generated bytes: {:?}", bytes);
            }
            None => panic!("Expected Some([u8; 32]), got None"),
        }
    }

    #[test]
    fn test_seed_phrase_to_bytes_without_seed_phrase() {
        // Case where seed_phrase is None
        let seed_phrase: Option<&str> = None;

        // Expected output: None
        let result = seed_phrase_to_bytes(seed_phrase);

        // Assert that result is None
        assert!(result.is_none(), "Expected None, got Some");
    }

    #[test]
    fn test_generate_keypair_with_seed() {
        let seed: [u8; 32] = [
            1, 2, 3, 4, 5, 6, 7, 8,
            9, 10, 11, 12, 13, 14, 15,16,
            17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32,
        ];

        let keypair_a = generate_keypair(Some(seed));
        let keypair_b = generate_keypair(Some(seed));

        assert_eq!(
            keypair_a.public(), keypair_b.public(),
            "Keypairs should be the same"
        );
    }

    #[test]
    fn test_generate_keypair_without_seed() {
        let keypair_a = generate_keypair(None);
        let keypair_b = generate_keypair(None);

        assert_ne!(
            keypair_a.public(), keypair_b.public(),
            "Keypairs should be different"
        );
    }

    #[test]
    fn test_save_keypair() {
        let provider = MockUserDirectoryProvider::new();
        let keypair = generate_keypair(None);

        let result = save_keypair(&keypair, &provider);

        // Check that the result is Ok
        assert!(result.is_ok(), "Failed to save keypair: {:?}", result);

        // Check that the key file has been created
        let file_path = DEFAULT_PATH.iter().fold(
            provider.get_temp_dir_path(), |path, component| path.join(component)
        );
        assert!(file_path.exists(), "Key file has not been created.");

        // Check that the content of the key file is correct
        let encoded_keypair_pbuf = keypair.to_protobuf_encoding().expect("Échec de l'encodage protobuf");
        let expected_encoded_keypair = STANDARD.encode(&encoded_keypair_pbuf);

        let saved_content = fs::read_to_string(file_path).expect("Échec de la lecture du fichier de clé");
        assert_eq!(
            saved_content, expected_encoded_keypair,
            "The contents of the key file do not match."
        );
    }

    #[test]
    fn test_generate_new_peer_id_with_seed_phrase() {
        let seed_phrase = Some("test_seed_phrase");
        let provider: MockUserDirectoryProvider = MockUserDirectoryProvider::new();

        let result = generate_new_keypair_and_peer_id(
            seed_phrase.as_deref(),
            &provider,
        );

        match result {
            Ok(peer_id) => {
                // Verify that a PeerId is generated
                assert!(peer_id.to_base58().len() > 0);
                println!("Generated Peer ID with seed: {}", peer_id);
            }
            Err(e) => panic!("Expected PeerId, got error: {}", e),
        }
    }

    #[test]
    fn test_generate_new_peer_id_without_seed_phrase() {
        let seed_phrase: Option<&str> = None;
        let provider = MockUserDirectoryProvider::new();

        let result = generate_new_keypair_and_peer_id(
            seed_phrase,
            &provider,
        );

        match result {
            Ok(peer_id) => {
                // Verify that a PeerId is generated
                assert!(peer_id.to_base58().len() > 0);
                println!("Generated Peer ID without seed: {}", peer_id);
            }
            Err(e) => panic!("Expected PeerId, got error: {}", e),
        }
    }
}
