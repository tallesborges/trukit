---
name: dotkit
description: "Use when working with the dotkit CLI (a fast single-binary Rust tool for Bulletin storage + DotNS naming on Paseo Asset Hub / pallet_revive) — deploying a static build dir to a .dot domain (merkleize, Bulletin upload, bind contenthash), registering an open-tier .dot name, resolving or setting a name's contenthash/text records, checking Bulletin quota, mapping SS58 to H160, or diagnosing a register/bind revert. Trigger phrases: deploy my app to a .dot with dotkit, dotkit deploy ./dist myapp.dot, register a .dot name, bind a CID to a .dot, why did dotkit register revert, set a manifest text record, dotkit deploy --register, what PoP tier does this name need."
---

# dotkit

Fast single-binary Rust CLI for the Polkadot Triangle/Trinity stack: **Bulletin** storage + **DotNS** naming (Asset Hub / `pallet_revive`). No Node/Bun, no `ipfs` daemon (native in-process UnixFS merkleization, byte-exact with Kubo 0.40.1). First-class command is `dotkit deploy`.

- **Binary:** `dotkit` on PATH, or build from source: `cargo build --release` → `./target/release/dotkit`.
- **Default env:** `paseo-next-v2` (resolves at `https://<name>.paseo.li`).

## Command surface

| Command | What it does |
|---|---|
| `deploy <dir> <domain.dot>` | Merkleize → Bulletin upload → bind `.dot` contenthash. Add `--register` to auto-register an open-tier name. |
| `bulletin store <file>` | Store one blob (≤2 MiB) on Bulletin. |
| `bulletin store-car <file.car>` | Store every block of a CARv1 so its root resolves. |
| `bulletin status [--address <ss58>]` | Bulletin authorization / quota for an account. |
| `asset-hub transfer <dest> <plancks>` | Send native PAS. |
| `asset-hub map` | Ensure the signer has an H160 mapping (`Revive.map_account`). |
| `asset-hub name resolve <name.dot>` | Name → contenthash CID. |
| `asset-hub name register <name.dot>` | Register an **open-tier** name (commit/reveal) to the signer. |
| `asset-hub name content set <name.dot> <cid>` | Bind a CID to a name's contenthash. |
| `asset-hub name content <name.dot>` | Read the raw contenthash record. |
| `asset-hub name text set <name.dot> <key> <value>` | Set a text record (e.g. `manifest`, `executable`). |
| `asset-hub name text get <name.dot> <key>` | Read a text record. |
| `account env` / `account whoami` | Print resolved env / prove signer + chain connectivity (shows SS58 + H160). |

**Global flags:** `--env <id>` (default `paseo-next-v2`), `--mnemonic`, `--derivation-path //x`, `-q/--quiet`.
**`deploy` flags:** `--register`, `--config <deploy.toml>`, `--input-car <file>`, `--kubo`.

## Signer & account model

- Default signer = a shared **dev account** (base of the standard dev phrase); its base derivation is the dev-mode DotNS owner on testnets. Override with `--mnemonic` (or `$MNEMONIC`, then `$DOTNS_MNEMONIC`) + `--derivation-path`.
- **Bulletin writes** use a random authorized **pool account** `//deploy/{0..9}` (derived from the dev phrase). These are Bulletin-authorized but **not funded on Asset Hub** — never use one as the DotNS owner signer (its `map_account`/bind will fail "balance too low").
- Every Revive write auto-runs `Revive.map_account` if the signer isn't mapped.

## DotNS naming rules & PoP tiers (verified on-chain)

The registrar's `classifyName` (on `POP_RULES`) gates a label by shape + base length:

| Label shape | Tier | classifyName status |
|---|---|---|
| Long base (e.g. `mycoolsite`, `dotshare-preview00`) | 0 | "Available to all" (open) |
| Shorter base + **exactly 2** trailing digits (`hostdiag91`) | 1 | "Requires Lite personhood verification" |
| Short base, no digits (`hostdiag`) | 2 | "Requires Full personhood verification" |
| Very short (`ab`) | 3 | Reserved |

- **A label must end in NO digits or EXACTLY 2 digits.** 1 or 3+ trailing digits → the contract reverts: `Name must have no digit suffix or exactly 2 digit suffix`.
- `dotkit name register` and `deploy --register` handle **tier 0 (open) only** — they bail on tier ≥1 with `requires PoP tier N (not open)`.
- Lite/Full names need a **personhood-verified signer** and a registration path that supports those tiers; dotkit does open-tier only. Get testnet personhood at `sudo.personhood.dev/personhood-faucet` (env "Next V2").

## Deploy workflow

```sh
# Deploy to a name you own (redeploy just updates the contenthash)
dotkit deploy ./dist myapp.dot

# First-time: register an open-tier name in the same run
dotkit deploy ./dist myapp.dot --register
```

`deploy` reads the Registry owner first: proceeds if you own it, errors if someone else does, and (with `--register`) registers an unregistered open-tier name before uploading. Then it merkleizes, uploads blocks to Bulletin (pool signer), binds the contenthash (owner signer), and prints the CID + `https://<name>.paseo.li`.

**Optional `deploy.toml`** (`--config <path>` or auto-detected `./deploy.toml`; unknown keys rejected):

```toml
[text]
manifest = "https://example.com/manifest.json"
executable = "worker.js"
```

Each `[text]` entry is written via `setText` after the bind. The build dir is never scanned for the config (its files get uploaded).

## Diagnosing reverts

dotkit surfaces the real EVM revert reason. Map it:

- `requires PoP tier N (not open)` → the name needs Lite/Full personhood; dotkit registers open-tier only.
- `Name must have no digit suffix or exactly 2 digit suffix` → rename (0 or 2 trailing digits).
- `custom error 0x14c417b5 …` echoing your H160 → not authorized (you don't own the node).
- `no reason returned (empty revert…)` → often an unmapped account or an address with no code; run `account whoami` / `asset-hub map`.
- `AccountUnmapped` / "balance too low" on map → fund the signer on Asset Hub (`faucet.polkadot.io/?parachain=1500`) then `asset-hub map`.

## Host / content contract

Deployed root must be **CIDv1 / dag-pb (or raw single-file) / sha2-256** with `index.html` at the directory root — the web host fails closed on any other multihash/codec. Native merkleization already produces exactly this (Kubo default).

## Hard rules

- **Open-tier only** for dotkit registration; Lite/Full names need external personhood verification.
- **Name digits:** none or exactly two, else the register reverts.
- **`<name>.paseo.li`** is the v2 gateway; `<name>.dot.li` points at the dead Summit chain — never use it for v2.
- **Secrets** via `$MNEMONIC` / `$DOTNS_MNEMONIC`, not `--mnemonic` in shell history.
- **`preview` env** has placeholder addresses — `name register` is not wired there.
- **Single blob > 2 MiB** is not yet supported (`bulletin store` bails; Kubo/native chunking keeps deploy blocks ≤256 KiB).
- **`--env` carries a matched set** — the Bulletin RPC and Asset Hub contract addresses go together; select an env, don't mix addresses across envs.
