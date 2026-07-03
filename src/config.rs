//! Optional deploy manifest (`deploy.toml`): DotNS metadata written alongside a
//! deploy. Currently drives text records (e.g. `manifest`, `executable`) set via
//! the resolver's `setText(node, key, value)`.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Parsed `deploy.toml`. Unknown fields are rejected so typos and unsupported
/// sections fail loudly rather than silently no-op.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployConfig {
    /// Text records to set on the domain, `key -> value`. `BTreeMap` keeps the
    /// write order stable across runs.
    #[serde(default)]
    pub text: BTreeMap<String, String>,
}

impl DeployConfig {
    /// Load the deploy config: from `explicit` when given (must exist), otherwise
    /// auto-detect `./deploy.toml` in the current directory (optional — an absent
    /// file yields an empty config). The build dir itself is never scanned, since
    /// its contents get uploaded wholesale.
    pub fn load(explicit: Option<&str>) -> Result<DeployConfig> {
        let path = match explicit {
            Some(p) => Some(PathBuf::from(p)),
            None => {
                let default = PathBuf::from("deploy.toml");
                default.is_file().then_some(default)
            }
        };
        let Some(path) = path else {
            return Ok(DeployConfig::default());
        };
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading deploy config {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing deploy config {}", path.display()))
    }
}
