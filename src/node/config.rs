use serde::Deserialize;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use tracing::debug;

/// Path of the configuration file when `--config` is not given.
pub const DEFAULT_CONFIG_PATH: &str = "Config.toml";

#[derive(Debug, Deserialize)]
pub struct Config {
    pub relayer: RelayerConfig,
}

#[derive(Debug, Deserialize)]
pub struct RelayerConfig {
    pub bootstrap_peer_ids: Vec<String>,
    pub bootstrap_nodes: Vec<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = fs::read_to_string(path)
            .map_err(|e| format!("Cannot read configuration file {}: {}", path.display(), e))?;
        Self::parse(path, &contents)
    }

    fn parse(path: &Path, contents: &str) -> Result<Self, Box<dyn std::error::Error>> {
        toml::from_str(contents).map_err(|e| {
            format!("Cannot parse configuration file {}: {}", path.display(), e).into()
        })
    }

    /// Loads the configuration file given with `--config`, or the one at
    /// `DEFAULT_CONFIG_PATH` when none was given.
    ///
    /// A missing file at the default path yields `Ok(None)`. A path given
    /// explicitly that cannot be read or parsed is an error, and so is a file
    /// at the default path that exists but cannot be read or parsed.
    pub fn load_optional(path: Option<&Path>) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        Self::load_or_default(path, Path::new(DEFAULT_CONFIG_PATH))
    }

    fn load_or_default(
        path: Option<&Path>,
        default_path: &Path,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        if let Some(path) = path {
            return Self::load(path).map(Some);
        }

        match fs::read_to_string(default_path) {
            Ok(contents) => Self::parse(default_path, &contents).map(Some),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                debug!(
                    "No configuration file at {}, going on without one",
                    default_path.display()
                );
                Ok(None)
            }
            Err(e) => Err(format!(
                "Cannot read configuration file {}: {}",
                default_path.display(),
                e
            )
            .into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn committed_config() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_CONFIG_PATH)
    }

    #[test]
    fn test_committed_config_parses() {
        let config = Config::load(&committed_config()).expect("Config.toml must parse");

        assert_eq!(config.relayer.bootstrap_peer_ids.len(), 2);
        assert_eq!(config.relayer.bootstrap_nodes.len(), 2);
    }

    #[test]
    fn test_missing_default_config_yields_none() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join(DEFAULT_CONFIG_PATH);

        let config = Config::load_or_default(None, &missing).unwrap();

        assert!(config.is_none());
    }

    #[test]
    fn test_existing_default_config_is_loaded() {
        let config = Config::load_or_default(None, &committed_config()).unwrap();

        let config = config.expect("the default file exists and must be loaded");
        assert_eq!(config.relayer.bootstrap_peer_ids.len(), 2);
    }

    #[test]
    fn test_invalid_default_config_is_an_error() {
        let dir = TempDir::new().unwrap();
        let invalid = dir.path().join(DEFAULT_CONFIG_PATH);
        fs::write(&invalid, "relayer = 1").unwrap();

        assert!(Config::load_or_default(None, &invalid).is_err());
    }

    #[test]
    fn test_unreadable_default_config_is_an_error() {
        // A directory at the default path exists but cannot be read as a file.
        let dir = TempDir::new().unwrap();

        assert!(Config::load_or_default(None, dir.path()).is_err());
    }

    #[test]
    fn test_missing_explicit_config_is_an_error() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("absent.toml");
        let default = dir.path().join(DEFAULT_CONFIG_PATH);

        let err = Config::load_or_default(Some(&missing), &default).unwrap_err();
        assert!(err.to_string().contains("absent.toml"));
        assert!(Config::load_optional(Some(&missing)).is_err());
    }

    #[test]
    fn test_invalid_explicit_config_is_an_error() {
        let dir = TempDir::new().unwrap();
        let invalid = dir.path().join("invalid.toml");
        fs::write(&invalid, "not = [toml").unwrap();

        let err = Config::load_or_default(Some(&invalid), &committed_config()).unwrap_err();
        assert!(err.to_string().contains("Cannot parse configuration file"));
        assert!(err.to_string().contains("invalid.toml"));
    }

    #[test]
    fn test_explicit_config_takes_precedence_over_the_default() {
        let dir = TempDir::new().unwrap();
        let explicit = dir.path().join("explicit.toml");
        fs::write(
            &explicit,
            "[relayer]\nbootstrap_peer_ids = [\"a\"]\nbootstrap_nodes = []\n",
        )
        .unwrap();

        let config = Config::load_or_default(Some(&explicit), &committed_config())
            .unwrap()
            .unwrap();

        assert_eq!(config.relayer.bootstrap_peer_ids, vec!["a".to_string()]);
    }
}
