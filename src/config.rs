//! Optional `deploy.toml`: DotNS metadata written alongside a deploy. Currently
//! just text records (e.g. `manifest`, `executable`) set via the resolver.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Parsed `deploy.toml`. Unknown fields are rejected so typos fail loudly.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployConfig {
    /// Text records to set on the domain (`key -> value`); `BTreeMap` for stable order.
    #[serde(default)]
    pub text: BTreeMap<String, String>,
}

impl DeployConfig {
    /// Load `deploy.toml` from `explicit` (must exist) or auto-detect `./deploy.toml`
    /// (absent → empty config). The build dir is never scanned — its files get uploaded.
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
