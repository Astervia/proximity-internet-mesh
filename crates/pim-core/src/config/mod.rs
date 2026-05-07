//! Configuration structures shared by the CLI and daemon.

mod defaults;
mod model;
mod peer;
mod peer_cleanup;

#[cfg(test)]
mod tests;

pub use model::*;
pub use peer::*;
pub use peer_cleanup::PeerCleanupConfig;

use std::path::Path;
use std::str::FromStr;

use crate::PimError;

impl Config {
    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self, PimError> {
        let content = std::fs::read_to_string(path).map_err(PimError::Io)?;
        content.parse()
    }

    /// Parse configuration from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, PimError> {
        s.parse()
    }

    /// Serialize configuration to a TOML string.
    pub fn to_toml_string(&self) -> Result<String, PimError> {
        toml::to_string_pretty(self).map_err(|e| PimError::Config(e.to_string()))
    }
}

impl FromStr for Config {
    type Err = PimError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        toml::from_str(s).map_err(|e| PimError::Config(e.to_string()))
    }
}
