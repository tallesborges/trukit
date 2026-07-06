//! Cross-run runtime-metadata cache and cache-seeded client connection. Keeps a
//! bespoke [`subxt::Config`] from re-downloading metadata on every block access,
//! and pre-seeds it from an on-disk copy so an unchanged runtime spec version is
//! served entirely from disk with no metadata download at all.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use subxt::config::RpcConfigFor;
use subxt::ext::codec::{Decode, Encode};
use subxt::metadata::{ArcMetadata, Metadata};
use subxt::rpcs::{LegacyRpcMethods, RpcClient};
use subxt::OnlineClient;

/// Per-client cache of runtime metadata keyed by spec version. subxt 0.50 fetches
/// metadata lazily on the first block access and offers the [`subxt::Config`] the
/// chance to cache it via [`subxt::Config::set_metadata_for_spec_version`]. The
/// default trait methods are no-ops, so a bespoke `Config` re-downloads metadata
/// on *every* `at_current_block`/`tx`/`wait_for_success` call; wiring this cache
/// in makes a reused client download each runtime's metadata exactly once.
///
/// Because every `dotkit` invocation is a fresh process, this in-memory cache is
/// additionally *pre-seeded* from the persistent on-disk cache (see
/// [`load_cached_metadata`]) before the client is built, so an unchanged runtime
/// spec version is served entirely from disk with no metadata download at all.
#[derive(Debug, Clone, Default)]
pub(crate) struct MetadataCache(Arc<RwLock<HashMap<u32, ArcMetadata>>>);

impl MetadataCache {
    pub(crate) fn get(&self, spec_version: u32) -> Option<ArcMetadata> {
        self.0.read().unwrap().get(&spec_version).cloned()
    }

    pub(crate) fn set(&self, spec_version: u32, metadata: ArcMetadata) {
        self.0.write().unwrap().insert(spec_version, metadata);
    }
}

/// Directory for the cross-run runtime-metadata cache. Honors `DOTKIT_CACHE_DIR`,
/// then `XDG_CACHE_HOME`, then `~/.cache`, filing metadata under `dotkit/metadata`.
/// Returns `None` when no location can be resolved — metadata is then fetched
/// fresh every run (still correct, just not cached).
fn metadata_cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DOTKIT_CACHE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("dotkit").join("metadata"));
    }
    let home = std::env::var_os("HOME").filter(|d| !d.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".cache")
            .join("dotkit")
            .join("metadata"),
    )
}

/// Filesystem-safe token identifying a chain endpoint (scheme stripped, every
/// non-alphanumeric byte mapped to `_`), so cached metadata for different
/// RPCs/envs never collides even when two chains happen to share a spec version.
fn cache_namespace(rpc_url: &str) -> String {
    let host = rpc_url.split_once("://").map_or(rpc_url, |(_, rest)| rest);
    host.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// On-disk path for a chain's metadata at a given spec version, or `None` when no
/// cache directory can be resolved.
fn metadata_cache_path(namespace: &str, spec_version: u32) -> Option<PathBuf> {
    Some(metadata_cache_dir()?.join(format!("{namespace}-v{spec_version}.scale")))
}

/// Fetch the raw `RuntimeMetadataPrefixed` SCALE bytes from the chain, mirroring
/// subxt's own selection: the newest non-unstable version via the
/// `Metadata_metadata_at_version` runtime API, falling back to the legacy
/// `Metadata_metadata`. The bytes are exactly what [`Metadata::decode_from`] (and
/// subxt internally) expect, so they can be persisted and reloaded verbatim —
/// crucially they include runtime-API descriptors (needed by `ReviveApi.*`),
/// unlike the trimmed-down `state_getMetadata` output.
async fn fetch_metadata_bytes<C: subxt::Config>(
    methods: &LegacyRpcMethods<RpcConfigFor<C>>,
) -> Result<Vec<u8>> {
    let latest_version = methods
        .state_call("Metadata_metadata_versions", None, None)
        .await
        .ok()
        .and_then(|res| <Vec<u32>>::decode(&mut &res[..]).ok())
        .and_then(|versions| versions.into_iter().filter(|v| *v != u32::MAX).max());

    if let Some(version) = latest_version {
        let params = version.encode();
        let resp = methods
            .state_call(
                "Metadata_metadata_at_version",
                Some(params.as_slice()),
                None,
            )
            .await
            .context("Metadata_metadata_at_version runtime call")?;
        // `Option<OpaqueMetadata>`; `OpaqueMetadata` encodes as a length-prefixed
        // byte blob, so decoding as `Option<Vec<u8>>` yields the inner
        // `RuntimeMetadataPrefixed` bytes directly.
        let bytes = <Option<Vec<u8>>>::decode(&mut &resp[..])
            .context("decoding Metadata_metadata_at_version response")?
            .context("chain returned no metadata for its latest metadata version")?;
        return Ok(bytes);
    }

    let resp = methods
        .state_call("Metadata_metadata", None, None)
        .await
        .context("Metadata_metadata runtime call")?;
    let bytes = <Vec<u8>>::decode(&mut &resp[..]).context("decoding Metadata_metadata response")?;
    Ok(bytes)
}

/// Download the chain's metadata bytes, decode them, and (best-effort) persist
/// them to `path` for reuse on the next run. A failed cache write never fails the
/// command.
async fn fetch_and_cache<C: subxt::Config>(
    methods: &LegacyRpcMethods<RpcConfigFor<C>>,
    path: Option<&Path>,
) -> Result<Metadata> {
    let bytes = fetch_metadata_bytes::<C>(methods).await?;
    let metadata =
        Metadata::decode_from(&bytes).context("decoding runtime metadata fetched from chain")?;
    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, &bytes);
    }
    Ok(metadata)
}

/// Resolve the runtime metadata for the chain reachable at `rpc`, preferring the
/// persistent on-disk copy keyed by `(endpoint, spec_version)` and only paying
/// for the (large) metadata download when the spec version isn't cached yet —
/// i.e. on first use or after a runtime upgrade. Keying by spec version keeps
/// this correct across upgrades: a bumped runtime is a cache miss and refetches.
/// The returned [`MetadataCache`] is pre-seeded so the subsequent client performs
/// no metadata download of its own.
async fn load_cached_metadata<C: subxt::Config>(
    rpc: &RpcClient,
    rpc_url: &str,
) -> Result<MetadataCache> {
    let methods = LegacyRpcMethods::<RpcConfigFor<C>>::new(rpc.clone());
    let spec_version = methods
        .state_get_runtime_version(None)
        .await
        .context("fetching runtime spec version")?
        .spec_version;

    let path = metadata_cache_path(&cache_namespace(rpc_url), spec_version);

    let metadata = match path.as_deref().and_then(|p| std::fs::read(p).ok()) {
        // A cached blob that no longer decodes (corrupt/partial) is ignored and refetched.
        Some(bytes) => match Metadata::decode_from(&bytes) {
            Ok(metadata) => metadata,
            Err(_) => fetch_and_cache::<C>(&methods, path.as_deref()).await?,
        },
        None => fetch_and_cache::<C>(&methods, path.as_deref()).await?,
    };

    let cache = MetadataCache::default();
    cache.set(spec_version, metadata.arc());
    Ok(cache)
}

/// Connect to `rpc_url` and build an [`OnlineClient`] whose config is pre-seeded
/// with the chain's runtime metadata from the persistent cache. A single RPC
/// connection is reused for the spec-version probe, any metadata fetch, and the
/// client itself.
pub(crate) async fn connect_with_cache<C, F>(
    rpc_url: &str,
    make_config: F,
) -> Result<OnlineClient<C>>
where
    C: subxt::Config,
    F: FnOnce(MetadataCache) -> C,
{
    let rpc = RpcClient::from_url(rpc_url)
        .await
        .with_context(|| format!("connecting to RPC {rpc_url}"))?;
    let cache = load_cached_metadata::<C>(&rpc, rpc_url)
        .await
        .with_context(|| format!("loading runtime metadata from {rpc_url}"))?;
    OnlineClient::from_rpc_client_with_config(make_config(cache), rpc)
        .await
        .with_context(|| format!("connecting to RPC {rpc_url}"))
}
