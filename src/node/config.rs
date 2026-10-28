use serde::Deserialize;
use std::fs;

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
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}
