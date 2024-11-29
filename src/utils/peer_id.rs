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
    /// Returns the user's home directory as a `PathBuf`, or `None` if it cannot
    /// be determined.
    fn get_user_home_dir(&self) -> Option<PathBuf> {
        UserDirs::new().map(|user_dirs| user_dirs.home_dir().to_path_buf())
    }
}

/// Takes an optional seed phrase and returns an optional 32-byte array of bytes derived from that phrase.
///
/// If the input seed phrase is `None`, the function will return `None`.
///
/// If the input seed phrase is `Some`, the function will hash the seed phrase using the SHA3-256 hash
/// algorithm, and return the resulting 32-byte array of bytes as an `Option<[u8; 32]>`. If the
/// resulting array of bytes cannot be converted into a 32-byte array, the function will return `None`.
fn seed_phrase_to_bytes(seed_phrase: Option<&str>) -> Option<[u8; 32]> {
    let seed = seed_phrase?;
    let mut hasher = Sha3_256::new();
    hasher.update(seed.as_bytes());
    let result = hasher.finalize();

    result.as_slice().try_into().ok()
}

/// Generates a cryptographic keypair.
///
/// If a secret key seed is provided, the function will use it to deterministically generate an Ed25519 keypair.
/// If no seed is provided, a new random Ed25519 keypair will be generated.
///
/// # Arguments
///
/// * `secret_key_seed` - An optional 32-byte array used to generate a deterministic keypair.
///
/// # Returns
///
/// A `Keypair` which contains the generated public and private keys.
///
/// # Panics
///
/// This function will panic if the provided seed is invalid for generating an Ed25519 keypair.
pub fn generate_keypair(secret_key_seed: Option<[u8; 32]>) -> Keypair {
    match secret_key_seed {
        Some(seed) => Keypair::ed25519_from_bytes(seed).unwrap(),
        None => Keypair::generate_ed25519(),
    }
}

    /// Saves a cryptographic keypair to a file on disk.
    ///
    /// The keypair is first encoded as a protobuf structure, then encoded as base64, and finally saved to
    /// a file on disk. The file is saved in the user's home directory, in a directory named `.chaincraft`,
    /// and has the filename `keypair.key`.
    ///
    /// If the directory `.chaincraft` does not exist, it will be created with the permissions `0o700`.
    /// If the file `keypair.key` does not exist, it will be created with the permissions `0o600`.
    ///
    /// # Arguments
    ///
    /// * `keypair` - The cryptographic keypair to be saved.
    /// * `provider` - An implementation of `UserDirectoryProvider` that provides the path to the user's home directory.
    ///
    /// # Returns
    ///
    /// A `Result` that indicates whether the keypair was successfully saved. If the home directory could not be
    /// determined, the function will return `Err("Home directory not found")`.
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

/// Reads a cryptographic keypair from a file in the user's home directory.
///
/// The keypair is stored as a base64-encoded protobuf structure in a file named `keypair.key`
/// within a directory named `.chaincraft`.
///
/// # Arguments
///
/// * `provider` - An implementation of `UserDirectoryProvider` that provides the path to
/// the user's home directory.
///
/// # Returns
///
/// A `Result` containing the `Keypair` if the file is successfully read and decoded.
/// Returns an error if the home directory cannot be determined, the file cannot be opened,
/// or the content cannot be decoded.
pub fn read_keypair_from_file<T: UserDirectoryProvider>(provider: &T) -> Result<Keypair, Box<dyn Error>> {
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

        Ok(Keypair::from_protobuf_encoding(&encoded_secret)?)
    } else {
        Err("Home directory not found".into())
    }
}

    /// Reads a cryptographic keypair from a file in the user's home directory and returns its PeerId.
    ///
    /// The file is read from the user's home directory in a directory named `.chaincraft`, and has the filename `keypair.key`.
    ///
    /// # Arguments
    ///
    /// * `provider` - An implementation of `UserDirectoryProvider` that provides the path to the user's home directory.
    ///
    /// # Returns
    ///
    /// A `Result` that indicates whether the PeerId was successfully generated.
    /// If the home directory could not be determined,
    /// the function will return `Err("Home directory not found")`.
    /// If the file `keypair.key` does not exist or is not a valid keypair,
    /// the function will return `Err("Error reading keypair")`.
pub fn generate_peer_id<T: UserDirectoryProvider>(provider: &T) -> Result<PeerId, Box<dyn Error>> {
    Ok(PeerId::from(read_keypair_from_file(provider)?.public()))
}

    /// Generates a new cryptographic keypair and saves it to the user's home directory.
    ///
    /// The generated keypair is saved to a file named `keypair.key` in a directory named `.chaincraft` in the user's home directory.
    ///
    /// The `seed_phrase` parameter is optional and can be used to generate a deterministic keypair.
    /// If `seed_phrase` is `None`, a new random keypair will be generated.
    ///
    /// # Arguments
    ///
    /// * `seed_phrase` - An optional seed phrase used to generate a deterministic keypair.
    /// * `provider` - An implementation of `UserDirectoryProvider` that provides the path to the user's home directory.
    ///
    /// # Returns
    ///
    /// A `Result` that indicates whether the keypair was successfully generated and saved.
    /// If the home directory could not be determined,
    /// the function will return `Err("Home directory not found")`.
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
