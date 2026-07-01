use anyhow::{bail, Result};

/// Resolved environment. `--env` selects the Bulletin RPC *and* the Asset Hub
/// contract addresses as one matched set so v1/v2 addresses can never drift.
///
/// Values verified 2026-07 against paritytech/bulletin-deploy assets/environments.json
/// and dotli-community packages/config. The registrar/pop-rules/registry addresses
/// are only known for paseo-next-v2 so far; the `preview` entries are placeholders
/// and `name register` on `preview` fails with a clear "invalid H160" error.
#[derive(Debug, Clone)]
pub struct Env {
    pub id: &'static str,
    pub bulletin_rpc: &'static str,
    pub asset_hub_rpc: &'static str,
    pub ipfs_gateway: &'static str,
    pub dotns_content_resolver: &'static str,
    pub registrar_controller: &'static str,
    pub pop_rules: &'static str,
    pub registry: &'static str,
    /// Public web gateway domain that resolves `<name>` in a browser. For Paseo
    /// v2 this is `paseo.li` (the old `dot.li` gateway pointed at the now-dead
    /// Summit chain). Empty when unknown.
    pub web_gateway: &'static str,
}

impl Env {
    pub fn resolve(id: &str) -> Result<Env> {
        Ok(match id {
            "paseo-next-v2" => Env {
                id: "paseo-next-v2",
                bulletin_rpc: "wss://paseo-bulletin-next-rpc.polkadot.io",
                asset_hub_rpc: "wss://paseo-asset-hub-next-rpc.polkadot.io",
                ipfs_gateway: "https://paseo-bulletin-next-ipfs.polkadot.io",
                dotns_content_resolver: "0x8A26480b0B5Df3d4D9b95adc24a5Ecb33A5b8F64",
                registrar_controller: "0x674b705268DAE369F0a7BE9cbaCDb928b8BA38C2",
                pop_rules: "0x4909bFb3f4Fd86244abD6430fDfA0Ce5C91aD0c4",
                registry: "0xa1b2b939E82b2ecE55Bd8a0E283818BfC1CA6CDc",
                web_gateway: "paseo.li",
            },
            "preview" => Env {
                id: "preview",
                bulletin_rpc: "wss://previewnet.substrate.dev/bulletin",
                asset_hub_rpc: "wss://previewnet.substrate.dev/asset-hub",
                ipfs_gateway: "https://previewnet.substrate.dev/ipfs",
                dotns_content_resolver: "0xBD003d5Dd04E68aC60d529a46AEfBdEf8941868C",
                registrar_controller: "",
                pop_rules: "",
                registry: "",
                web_gateway: "",
            },
            other => bail!("unknown --env '{other}' (known: paseo-next-v2, preview)"),
        })
    }
}
