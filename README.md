# dotkit

A fast single-binary Rust CLI for the Polkadot **Triangle/Trinity** ecosystem — **Bulletin** storage and **DotNS** naming (Asset Hub / `pallet_revive`).

The first-class command is `dotkit deploy`, a native replacement for the existing Node-based deploy + `.dot` naming CLIs: it merkleizes a build directory, uploads the DAG to the Bulletin chain, and binds the content CID to a `.dot` domain — all from one static binary with no Node/Bun runtime and no `ipfs` daemon.

## Why

- **One static binary** — no `node_modules`, no runtime install, fast cold-start.
- **Native merkleization** — an in-process UnixFS encoder that reproduces Kubo's CIDv1 layout (CIDv1, raw leaves, sha2-256, 256 KiB balanced chunks, wrap-with-directory). No `ipfs` binary required. Golden-tested for byte-exact CID parity against Kubo 0.40.1.
- **Env-matched** — `--env` selects the Bulletin RPC *and* the Asset Hub contract addresses as one set, so the v1/v2 address split can't drift.

## Install

Requires a recent stable Rust toolchain.

```sh
git clone git@github.com:tallesborges/dotkit.git
cd dotkit
cargo build --release
# binary at ./target/release/dotkit
```

## Quickstart

Deploy a built site to a `.dot` domain you own:

```sh
dotkit deploy ./dist myapp.dot
```

This merkleizes `./dist`, uploads every block to Bulletin, sets the contenthash on `myapp.dot`, and prints the gateway + `https://myapp.paseo.li` URL.

Register an open-tier name first if you need one:

```sh
dotkit asset-hub name register myapp.dot
```

## Command surface

| Command | What it does |
|---|---|
| `deploy <dir> <domain.dot>` | Merkleize → Bulletin upload → bind `.dot` contenthash (the MVP flow). |
| `bulletin store <file>` | Store a single blob (≤2 MiB) on Bulletin. |
| `bulletin store-car <file.car>` | Store every block of a CARv1 so its root resolves. |
| `bulletin status [--address <ss58>]` | Show authorization / quota for an account. |
| `asset-hub transfer <dest> <plancks>` | Send native PAS on Asset Hub. |
| `asset-hub map` | Ensure the signer has an H160 mapping (`Revive.map_account`). |
| `asset-hub name resolve <name.dot>` | Resolve a name to its contenthash CID. |
| `asset-hub name register <name.dot>` | Register an open-tier name (commit/reveal) to the signer. |
| `asset-hub name content set <name.dot> <cid>` | Bind a CID to a name's contenthash. |
| `asset-hub name content <name.dot>` | Read a name's raw contenthash record. |
| `account env` | Print the resolved environment config. |
| `account whoami` | Derive the signer and prove Asset Hub + Bulletin connectivity. |

### `deploy` flags

- `--input-car <file>` — deploy a pre-built CAR instead of merkleizing.
- `--kubo` — merkleize with the external `ipfs` binary instead of the native encoder (fallback).

### Global flags

- `--env <id>` — target environment (default `paseo-next-v2`).
- `--mnemonic <phrase>` — signer mnemonic. Falls back to `$MNEMONIC`, then `$DOTNS_MNEMONIC`; defaults to a shared dev account on testnets.
- `--derivation-path <path>` — Substrate derivation path (e.g. `//Alice`).
- `-q`, `--quiet` — suppress step/detail output; only errors are printed (useful in CI/scripts).

## Environments

| `--env` | Bulletin + Asset Hub | Notes |
|---|---|---|
| `paseo-next-v2` (default) | Paseo Next v2 | Full support; resolves at `<name>.paseo.li`. |
| `preview` | PreviewNet | Partial — `asset-hub name register` is not yet wired. |

## How merkleization stays Kubo-compatible

`dotkit deploy` builds the same content DAG that `ipfs add -r --cid-version=1 --raw-leaves --hidden` would, using [`rust-unixfs`](https://crates.io/crates/rust-unixfs)'s `FileAdder` + `BufferingTreeBuilder`. Files are added in lexicographic path order (hidden files included), chunked at 256 KiB, hashed with sha2-256, and wrapped in a directory root — the exact defaults Kubo uses for CIDv1. The produced blocks are stored on Bulletin keyed by their own content hash, so the root CID resolves on any IPFS gateway.

Parity is enforced by golden tests, including live cross-checks against `ipfs` when present:

```sh
# unit golden vectors (no ipfs needed)
cargo test merkle

# compare our root vs kubo for any directory (needs ipfs on PATH)
DOTKIT_COMPARE_DIR=./dist cargo test -- --ignored compare_env
```

## Status

The `deploy` MVP is built and live-verified end-to-end on `paseo-next-v2`. Native merkleization (dropping the Kubo shell-out) is complete and golden-tested. Remaining work: config files + text records, non-open register tiers, and a chunked path for single blobs larger than 2 MiB.
