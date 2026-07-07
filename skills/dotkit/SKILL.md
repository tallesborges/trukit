---
name: dotkit
description: "Use when working with the dotkit CLI (a fast single-binary Rust tool for Bulletin storage + DotNS naming on Paseo Asset Hub / pallet_revive) — deploying a static build dir to a .dot domain (merkleize, Bulletin upload, bind contenthash), registering an open-tier .dot name, looking up who owns a name or whether it's available, transferring a name you own, resolving or setting a name's contenthash/text records, verifying a CID resolves on the gateway, checking or granting Bulletin quota, checking a PAS balance, mapping SS58 to H160, emitting machine-readable --json, or diagnosing a register/bind revert. Trigger phrases: deploy my app to a .dot with dotkit, dotkit deploy ./dist myapp.dot, register a .dot name, who owns this .dot, transfer a .dot to someone, bind a CID to a .dot, verify a CID resolves, authorize an account for Bulletin, why did dotkit register revert, set a manifest text record, dotkit deploy --register, what PoP tier does this name need."
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
| `bulletin verify <cid>` | Check a CID actually resolves on the env's IPFS gateway (live HTTP probe). |
| `bulletin authorize [--address <ss58>] [--transactions N] [--bytes N]` | Grant an account Bulletin storage quota. Signer needs **Authorizer** privileges (pass `--mnemonic`); not the pool. |
| `asset-hub transfer <dest> <plancks>` | Send native PAS. |
| `asset-hub map` | Ensure the signer has an H160 mapping (`Revive.map_account`). |
| `asset-hub name resolve <name.dot>` | Name → contenthash CID. |
| `asset-hub name owner-of <name.dot>` (alias `oo`) | Whether a name is registered and who owns it (H160). |
| `asset-hub name lookup <name.dot>` | Read-only overview: owner, required tier + status, base price, contenthash. |
| `asset-hub name register <name.dot>` | Register a name (commit/reveal) to the signer — open, or Lite/Full with a personhood-verified signer. |
| `asset-hub name transfer <name.dot> <to>` | Transfer a name you own to `<to>` (0x H160 or SS58); pays the quoted friction fee. |
| `asset-hub name content set <name.dot> <cid>` | Bind a CID to a name's contenthash. |
| `asset-hub name content <name.dot>` | Read the raw contenthash record. |
| `asset-hub name text set <name.dot> <key> <value>` | Set a text record (e.g. `manifest`, `executable`). |
| `asset-hub name text get <name.dot> <key>` | Read a text record. |
| `account env` / `account whoami` | Print resolved env / prove signer + chain connectivity (shows SS58 + H160). |
| `account info` | Show the signer's Asset Hub native (PAS) balance. |
| `bulletin pool init [--accounts N] [--force]` / `status` / `authorize [--transactions N] [--bytes N]` | Manage a **private per-machine** Bulletin upload pool (`~/.dotkit/pool.toml`, `0600`; derived `//deploy/N`). `status` shows each account's **on-chain** auth + quota with an `N/M authorized` rollup (honors `--pool`, so `--pool shared` inspects the shared pool). `authorize` batch-authorizes all accounts via `//Alice` (`utility.batch_all`, idempotent). `deploy`/`store` use the pool by default (override with `--pool local\|shared`). Testnet-only. |

**Global flags:** `--env <id>` (default `paseo-next-v2`), `--mnemonic`, `--derivation-path //x`, `--pool <local|shared>` (Bulletin upload pool; default: private `~/.dotkit` pool if a keystore exists, else shared), `-q/--quiet`, `--json` (one machine-readable JSON object per command; errors become `{"error": …}` on stderr).
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
- `dotkit name register` and `deploy --register` handle **open (0) and personhood-gated Lite (1) / Full (2)**; **Reserved (3)** is rejected (governance-only). For Lite/Full, dotkit pre-checks the owner's `personhoodStatus(owner, "dotns")` on the AH precompile (`0x…0a010000`) and bails **before committing** if the signer's tier is too low.
- Lite/Full names need a **personhood-verified signer** (Full satisfies Lite). Get testnet personhood at `sudo.personhood.dev/personhood-faucet` (env "Next V2"); the signer must also be funded + H160-mapped on Asset Hub. Note: People-chain personhood is **not** auto-bridged — bind it to the `dotns` context via `sudo.personhood.dev/dotns-bootstrap` first.

## Deploy workflow

```sh
# Deploy to a name you own (redeploy just updates the contenthash)
dotkit deploy ./dist myapp.dot

# First-time: register the name in the same run (open, or Lite/Full if the signer is verified)
dotkit deploy ./dist myapp.dot --register
```

`deploy` reads the Registry owner first: proceeds if you own it, errors if someone else does, and (with `--register`) registers an unregistered name (open, or Lite/Full if the signer has the personhood) before uploading. Then it merkleizes, uploads blocks to Bulletin (pool signer), binds the contenthash (owner signer), and prints the CID + `https://<name>.paseo.li`.

**Optional `deploy.toml`** (`--config <path>` or auto-detected `./deploy.toml`; unknown keys rejected):

```toml
[text]
manifest = "https://example.com/manifest.json"
executable = "worker.js"
```

Each `[text]` entry is written via `setText` after the bind. The build dir is never scanned for the config (its files get uploaded).

## Diagnosing reverts

dotkit surfaces the real EVM revert reason. Map it:

- `requires Lite/Full personhood, but the signer … has NoStatus` → the name is personhood-gated; use a verified signer (`sudo.personhood.dev/personhood-faucet`, env Next V2) or pick an open (long-base) name. dotkit bails here **before** committing.
- `Name must have no digit suffix or exactly 2 digit suffix` → rename (0 or 2 trailing digits).
- `custom error 0x14c417b5 …` echoing your H160 → not authorized (you don't own the node).
- `no reason returned (empty revert…)` → often an unmapped account or an address with no code; run `account whoami` / `asset-hub map`.
- `AccountUnmapped` / "balance too low" on map → fund the signer on Asset Hub (`faucet.polkadot.io/?parachain=1500`) then `asset-hub map`.

## Host / content contract

Deployed root must be **CIDv1 / dag-pb (or raw single-file) / sha2-256** with `index.html` at the directory root — the web host fails closed on any other multihash/codec. Native merkleization already produces exactly this (Kubo default).

## Hard rules

- **Open + Lite/Full** registration (Reserved rejected). Lite/Full need a personhood-verified signer; dotkit pre-checks `personhoodStatus` and bails early if the signer's tier is too low.
- **Name digits:** none or exactly two, else the register reverts.
- **`<name>.paseo.li`** is the v2 gateway; `<name>.dot.li` points at the dead Summit chain — never use it for v2.
- **Secrets** via `$MNEMONIC` / `$DOTNS_MNEMONIC`, not `--mnemonic` in shell history.
- **`preview` env** has placeholder addresses — `name register`, `name transfer`, and `lookup` price/tier are not wired there (registrar/registry/NFT addresses only exist for `paseo-next-v2`).
- **Name transfer** pays the registrar's quoted friction fee (0 for same-tier/upward moves, a fee for downward moves); only the current NFT owner can transfer, and the recipient `<to>` is a `0x` H160 or SS58 address.
- **`bulletin authorize`** needs a signer that holds Bulletin **Authorizer** privileges (pass `--mnemonic`); the default storage pool cannot authorize and the chain returns `BadOrigin`.
- **`--json`** makes every command print one JSON object to stdout (read commands like `name owner-of`/`lookup`, `bulletin verify`, `account info` are read-only and script-friendly); on failure it prints `{"error": …}` to stderr.
- **Single blob > 2 MiB** is not yet supported (`bulletin store` bails; Kubo/native chunking keeps deploy blocks ≤256 KiB).
- **`--env` carries a matched set** — the Bulletin RPC and Asset Hub contract addresses go together; select an env, don't mix addresses across envs.
