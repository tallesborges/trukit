# dotkit — a Rust CLI for the Triangle/Trinity ecosystem (Ship-First Plan)

`dotkit` is a single fast Rust binary — a personal umbrella CLI whose subcommands operate across the
three ecosystem surfaces (**Bulletin** storage, **DotNS** naming on Asset Hub, **People** / statement
store). The **first shipped subcommand is `dotkit deploy`**, which replaces the existing Node-based
Bulletin deploy + `.dot` naming CLIs. Motivation: faster cold-start (no Node/Bun boot, no npm install), a
single static binary, one codebase you fully understand, and room to bolt on the other ops you actually use.

Name chosen: **`dotkit`** ("dot" + "kit" = a Polkadot toolkit) — crates.io-free, signals a personal
multi-tool over the `.dot`/Polkadot surfaces, and drops the earlier `tru*`/`tri*` prefix, which
collided with the official **Trinity** (né trUAPI) brand. (Renamed from `trukit` 2026-07-02.)

> Scope of this plan: the `deploy` subcommand at wire level + a demoable slice order, structured so the
> other surfaces slot in as subcommands later. Grounded in the existing Node deploy tooling and `.dot` naming client.

## Command surface (subcommand map — organized by chain/surface)

Build as a top-level `clap` app with subcommands from day one; each surface is its own module so adding
commands later is cheap and rename-free. MVP ships only `deploy`; the rest are the planned toolbelt.

- `dotkit deploy <dir> <domain.dot>` — **MVP.** Composite: merkleize → Bulletin upload → DotNS `setContenthash`. Spans Bulletin + Asset Hub.
- **Bulletin surface** (`dotkit bulletin …`) — `store <file>`, `status`/quota (`Authorizations`), `authorize <acct>`; direct blob/CAR ops without a full deploy.
- **Asset Hub surface** (`dotkit asset-hub …`) — `transfer`, `map`, plus DotNS naming nested under `asset-hub name …` (`resolve`, `register` (commit/reveal + PoP), `content set|get`, `text get|set`, `pop`, `publish`).
- **Shared** (`dotkit account …`) — `env` config + `whoami` connectivity across chains; `--env` selection used by every subcommand.

# Goals
- One Rust binary, `dotkit`, with `deploy` as the first working subcommand: `dotkit deploy <build-dir> <domain.dot>`
  merkleizes, uploads to Bulletin, and binds the content CID to an existing `.dot` domain — resolving on `https://<name>.paseo.li`.
- Cold-start noticeably faster than the Node CLIs; no runtime install step.
- Env-aware (paseo-next-v2 default) so Bulletin RPC and Asset Hub contract addresses never drift; every subcommand shares the env resolver.
- Extensible subcommand structure so Bulletin / DotNS / Statement-Store ops slot in without a rewrite.

---

## How the two SDKs actually work (wire level)

A deploy is really **two independent byte layers + one contract write**:

### Layer A — Merkleize the build dir → content CID (goes into DotNS)

- Kubo path: `ipfs add -Q -r --hidden --cid-version=1 --raw-leaves --pin=false <dir>` then
  `ipfs dag export <cid>` → CAR.
- JS path: `ipfs-unixfs-importer` with `cidVersion:1, rawLeaves:true, wrapWithDirectory:true`,
  files walked lexicographically for determinism. CAR written via `@ipld/car`.
- Result: CIDv1 **dag-pb** (`0x70`) wrapped-dir root; leaves are CIDv1 **raw** (`0x55`).
- This **content CID** is what gets bound to the `.dot` name.

### Layer B — Upload the DAG blocks to Bulletin → content CID resolves

**✅ VERIFIED (Spike B, on-chain) — the real model, which corrects an earlier misread of the JS:**
Bulletin makes a content CID resolvable by storing **each IPLD block of the DAG individually, keyed by
its own CID**. There is NO "slice the CAR bytes into 2 MiB chunks + build a synthetic UnixFS storage
root." Proof: host-playground.dot's dag-pb dir root block (`0x31ece6…`) is stored per-CID
(`TransactionByContentHash[0x31ece6…]=[761940,0]`) and the gateway serves it at `?format=raw` with a
matching sha256. Since Kubo chunks files into ≤256 KiB blocks, every block already fits one ≤2 MiB extrinsic.

- Chain: Bulletin parachain (`paseo-next-v2` = `wss://paseo-bulletin-next-rpc.polkadot.io`, para 1501).
- Parse the CARv1 (`iroh-car`), iterate `(Cid, block)` pairs, and store **each block** via
  **`TransactionStorage.store_with_cid_config`** with `{ cid: { codec: <block's codec: 0x55 raw or 0x70 dag-pb>,
  hashing: Sha2_256 }, data: block }`. The CAR root = the content CID = what binds to DotNS and resolves.
- Chain cap: `MaxTransactionSize = 2 MiB` (verified). Guard any block > 2 MiB (shouldn't occur with Kubo).
- Idempotent: skip a block whose `TransactionByContentHash[sha256(block)]` is already `Some`.
- **Built and verified in `dotkit bulletin store-car` (Slice 2b):** a fresh Kubo CAR → per-block store →
  the content CID + its named files resolve on the gateway. `stored=N skipped=M` idempotent re-runs.
- Superseded JS complexity NOT reproduced (correctly): 2 MiB CAR-slicing, the synthetic DAG-PB "storage
  root", `MAX_FILE_SIZE = 8 MiB`, batch/dense-nonce/mortality machinery. Single-signer sequential
  wait-finalized store is sufficient. Bulletin's custom signed extensions require a bespoke subxt `Config`.

### Layer C — Bind content CID to the `.dot` name (DotNS on Asset Hub)

- Chain: Asset Hub Revive/EVM (`paseo-next-v2` = `wss://paseo-asset-hub-next-rpc.polkadot.io`, para 1500).
- `node = viem.namehash("<label>.dot")` (ENS namehash).
- contenthash = `0xe301` + raw CID bytes (`encodeContenthash`).
- ABI: `setContenthash(bytes32 node, bytes hash)` on `DOTNS_CONTENT_RESOLVER`.
- Submission is **NOT** eth_sendTransaction. Calldata is ABI-encoded (viem `encodeFunctionData`),
  dry-run via `ReviveApi.call(...)`, then written as **`pallet_revive` `Revive.call`** extrinsic
  `{ dest, value, weight_limit, storage_deposit_limit, data }`.
- Optional `Publisher.publish(label)` (`labelhash = keccak256(label)`) — paseo-next-v2 only.

### Account / crypto model (shared)

- **sr25519** Substrate accounts from mnemonic/URI (PAPI signer via
  `getPolkadotSigner(pub,"Sr25519",sign)`).
- Bulletin pool = a shared, pre-authorized Bulletin storage pool of derived sub-accounts from a shared
  pool mnemonic (or `DEV_PHRASE`), `sr25519CreateDerive(miniSecret)`. Direct mode = single mnemonic
  with an optional derivation path.
- Every Revive write first ensures the signer's SS58↔H160 mapping:
  `ReviveApi.address(ss58)`, `Revive.OriginalAccount`, else `Revive.map_account()`.
- Bulletin authorization (quota) = `TransactionStorage.authorize_account({who,transactions,bytes})`
  by an authorizer (`//Alice` on testnets); read via `TransactionStorage.Authorizations`.

### Rust crate mapping (verified against crates.io + deep research, 2026-07)

| Area | Crate(s) | Ver | Status | Notes |
|---|---|---|---|---|
| Chain RPC + extrinsics + runtime APIs | `subxt` | 0.50.1 | ✅ active (paritytech) | supports signed extrinsics, storage/const reads, `runtime_apis().call(...)`; static or dynamic metadata |
| sr25519 signing + derivation | `subxt-signer` | 0.50.1 | ✅ active | `SecretUri` + junctions (copies `sp_core` logic); **golden-test the derived SS58 vs JS** |
| SCALE codec | `parity-scale-codec` | 3.7.x | ✅ active | |
| mnemonic → entropy | `bip39` | 2.2.x | ✅ active | |
| EVM ABI + keccak | `alloy-sol-types`, `alloy-primitives` (+ `alloy-dyn-abi` if runtime ABI) | 1.6.x | ✅ active (alloy-rs) | `keccak256`, `FixedBytes`, `Address`; **ethers-rs archived Oct-2024, ethabi stale — avoid** |
| ENS namehash | — (hand-roll on `keccak256`) | — | n/a | no maintained namehash crate; EIP-137 is ~10 lines |
| CID | `cid` | 0.11.3 | ✅ active (multiformats) | `Cid::new_v1(codec, mh)` |
| multihash | `multihash` + `multihash-codetable` | 0.19.x / 0.2.x | ✅ active | `Code::Sha2_256` (0x12), `Code::Blake2b256` (0xb220) |
| digests | `sha2`, `blake2` | RustCrypto | ✅ active | |
| **UnixFS / DAG-PB importer** | `rust-unixfs` (dariusc93/rust-ipfs) | 0.6.0 (Jun-2026) | 🟡 usable, solo-maintainer | **Real standalone encoder** (`FileAdder` + `BufferingTreeBuilder`); defaults already match Kubo: CIDv1, raw leaves, sha2-256, **256 KiB fixed chunker**, balanced DAG **bf=174** (go-ipfs 0.6), HAMT sharding, wrap-with-dir. Ships Kubo-0.40.1 pinned interop vectors — but live compare tests are `#[ignore]`d, so **golden-test + pin/vendor the SHA**. Deps are lean (no tokio/libp2p). |
| DAG-PB codec (if hand-building links) | `ipld-dagpb` | 0.2.x | 🟡 usable | codec only, not a full importer |
| CARv1 read/write | `iroh-car` | 0.5.1 | 🟡 usable (n0) | `CarReader`/`CarWriter`; last release Oct-2024 but spec-stable |
| CLI / async | `clap`, `tokio` | current | ✅ | |

**Correction vs the deep-research report:** it assumed DotNS calls go through `pallet_revive::eth_transact`
(the Ethereum-RPC path). Our verified consumers (the existing Node deploy tooling + `.dot` naming client) instead use the native
**`Revive.call`** extrinsic after a **`ReviveApi.call`** dry-run, with SS58↔H160 mapping. Match the existing
tools: `Revive.call`, not `eth_transact`. Also `pallet_revive` is still "experimental" and shifts call
indices across upgrades → prefer **dynamic metadata for the revive path** (pin/print runtime spec at startup).

---

## Trust boundary — reproduce the protocol, discard the implementation

The existing Node deploy tooling and `.dot` naming client are over-engineered and not a reference to copy. Treat
their code as *hints*, and verify every claim against ground truth that exists **outside** the JS:
chain metadata (`dot <chain>.const/inspect`), on-chain contract code/ABI (`ReviveApi.code`), and an
IPFS gateway round-trip. That method already caught a real bug — the "8 MiB fast path" is a stale JS
guard that's *looser than the chain allows* (real cap is 2 MiB, verified). Do not inherit that class
of cruft.

### MUST reproduce (protocol-enforced — independently verifiable)
- **Bulletin extrinsic shape**: `TransactionStorage.store_with_cid_config { cid:{codec,hashing}, data }`;
  chunk CIDs = CIDv1 raw (`0x55`) / sha2-256. (verify: chain metadata + read stored blob back via gateway)
- **2 MiB per-tx cap** (`MaxTransactionSize`) — hard chain limit, not a JS choice.
- **Bulletin quota**: `TransactionStorage.authorize_account` / `Authorizations` gate. (verify: metadata)
- **Content addressing**: a *valid* UnixFS/DAG-PB DAG so IPFS gateways resolve it. (verify: gateway fetch, NOT byte-match to JS)
- **DotNS write**: `node = namehash("<label>.dot")`, contenthash = `0xe301`+CID bytes,
  `setContenthash(bytes32,bytes)` ABI, submitted via `pallet_revive Revive.call` after a `ReviveApi.call`
  dry-run; SS58↔H160 via `ReviveApi.address` / `Revive.map_account`. (verify: contract ABI + on-chain code)
- **Registration** (later): commit/reveal is contract-enforced (`makeCommitment`, `minCommitmentAge`).
- **sr25519** signing + derivation for the shared pool.

### DISTRUST / redesign (JS implementation choices, not protocol)
- The `MAX_FILE_SIZE = 8 MiB` guard — wrong; drop it.
- The 10-account pool + heavy nonce/batch/reconnect/mortality/fallback retry machinery — much of it
  is throughput/recovery, not protocol. Nuance (per review): a **sequential, wait-for-finalized**
  single-signer loop is safe and correct for the demo; a **concurrent** loop is NOT safe without local
  dense-nonce management (same-account races produce stale/future-nonce rejects). So: start sequential;
  reintroduce dense nonces (and only then batching/probing) when throughput demands it, not by default.
  Note the pool is also a *quota* mechanism (authorization is per-account), not purely a PAPI workaround.
- Ordered/incremental CAR, section packing, `2 MiB-1 KiB` chunker, reproducible timestamps — premature optimization; skip.
- Multi-layer probe/verify/reprobe passes — keep one honest post-upload verification, not three.
- Store-contract CID caching (`--cache`), gh-pages mirror, telemetry, bug-report scrubbing, host-session allowance path — all out of scope.

---

## Host compatibility contract (consumption side — verified across all 4 hosts)

Checked what each host actually does when it turns a `.dot` contenthash into rendered content:
the desktop host, the web host, and the iOS and Android mobile hosts. They are strikingly uniform.

**Universal, all four hosts agree:**
- DotNS contenthash = **EIP-1577 IPFS**: `0xe301` + raw CID bytes. Each strips/decodes it (the desktop and web hosts via `@ensdomains/content-hash`; the mobile hosts strip the `0xe3 0x01` prefix directly and reject any non-IPFS prefix).
- `node = namehash("<label>.dot")`, ENS-style keccak.
- Fetch via the env **IPFS HTTP gateway**: try the CID raw, else re-fetch `?format=car` with `Accept: application/vnd.ipld.car`.
- Parse **CARv1, single root**; traverse **UnixFS/DAG-PB**.
- Root codec ∈ { **dag-pb `0x70`** (directory), **raw `0x55`** (single file) }.
- Directory must contain **`index.html`**; extensionless paths → `<path>/index.html`; SPA fallback serves root `index.html` for main-frame navigations.
- **No manifest/signature/integrity gate on loading.** `manifest`/`executable` text records are optional branding only — the app root lives in the contenthash, not in any manifest JSON.

**The one hard new constraint (the web host is the strictest verifier):**
- The web host **hash-verifies the CAR root and every block against the requested CID** and accepts **only sha2-256 (`0x12`) or blake2b-256 (`0xb220`)** multihashes and dag-pb/raw codecs — anything else **fails closed**. The desktop and mobile hosts are looser (parse without strict multihash checks), so **sha2-256 satisfies all four**. → **Use CIDv1 + dag-pb dir + sha2-256, with `index.html` at the root.** That's also the Kubo default, so it doubles as our reproducibility target.

**Separate regime — preimages (product icons / host-API blobs), NOT the app CAR:**
- All four hosts hardcode **CIDv1 + raw codec + blake2b-256** for `preimageLookup`/`hashToCid`
  (the desktop host, the iOS host, and the android host; Bulletin `store` returns `blake2b256(data)`).
  This is what the existing deploy tooling's "icons must be blake2b" comment meant.
  → Only relevant if the tool later uploads blobs resolved via the host preimage API (e.g. manifest icons).
  The main site CAR stays sha2-256; icons, if any, must be blake2b-256/raw.

**Gateways (env-specific, from config, not hardcoded):** paseo-next-v2 = `https://paseo-bulletin-next-ipfs.polkadot.io`; older/desktop default = `https://paseo-ipfs.polkadot.io`.

---

# Goals
- One Rust binary: `deploy <build-dir> <domain.dot>` that merkleizes, uploads to Bulletin, and
  binds the content CID to an existing `.dot` domain — resolving on `https://<name>.paseo.li`.
- Cold-start noticeably faster than the Node CLIs; no runtime install step.
- Env-aware (paseo-next-v2 default) so Bulletin RPC and Asset Hub contract addresses never drift.

# Non-goals (initial)
- Everything except `dotkit deploy`. The `bulletin` / `name` / `statement` / `account` subcommands are
  planned but post-MVP — the architecture reserves space for them; the MVP implements only `deploy`.
- The **People / statement-store** surface specifically: its on-chain submission path (pallet/RPC) is
  **unverified** — treat as research-first, don't design it into the MVP.
- Byte-identical CAR/CID reproduction vs Kubo — target a *valid, gateway-resolvable* DAG, not
  bit-for-bit parity (reproducibility is a later polish concern).
- Domain registration (commit/reveal + PoP gating), Publisher listing, encryption, gh-pages mirror,
  pool bootstrap — all deferred past MVP (they become `dotkit name register` / flags later).
- Replacing the mobile/host-session allowance path — out of scope.

# Design principles
- User journey drives order: the daily action is **redeploy to a domain I already own**, so that path ships first.
- `--env` drives both chains together so Bulletin RPC and Asset Hub contract addresses can't drift (our own invariant — the v1/v2 address split is a real footgun, independent of how the JS tools handle it).
- Ground truth is the chain + contracts + IPFS gateway, never the JS tools. Verify every borrowed detail independently; the JS is over-engineered and has at least one confirmed wrong constant.
- Verify against ground truth: prove each CID by fetching through the real IPFS gateway, not by trusting our own encoder.
- Reuse the already-authorized testnet pool/signer so the MVP needs no on-chain bootstrap.

# User journey
1. Dev builds their app (`dist/`) and runs `deploy ./dist myapp00.dot`.
2. Tool merkleizes → content CID; uploads CAR to Bulletin; sets contenthash on the domain.
3. Tool prints the CID + `https://myapp00.paseo.li` and the dev opens it.

# Status / Already shipped (✅ — updated 2026-07)

**The deploy MVP is BUILT and live-verified end-to-end on paseo-next-v2.** The tool is `dotkit`, now
committed and pushed to `github.com/tallesborges/dotkit` (public). What's done and verified against the
live chain + gateway:

- ✅ **Slice 0** — `dotkit account whoami/env/map/transfer`: subxt 0.50 clients for Bulletin + Asset Hub,
  sr25519 signer (derived pool sub-accounts), H160 mapping via `ReviveApi.address`. Pinned metadata snapshots.
- ✅ **DotNS read** (`name resolve`/`name content`) — namehash → `contenthash(bytes32)` via `ReviveApi.call`
  → decode `0xe301` → CIDv1. Golden-tested vs host-playground.dot.
- ✅ **Slice 2 / 2b** (`bulletin status`/`store`/`store-car`) — signed `store_with_cid_config` via a bespoke
  `BulletinConfig`; **CAR uploaded per-block so the content CID resolves on the gateway** (Spike A/B/C settled).
- ✅ **DotNS write** (`name content set`) — signed `setContenthash` via a bespoke **`AssetHubConfig`** (17
  tx extensions), H160-mapping, dry-run gating, read-back verify.
- ✅ **Slice 3** (`dotkit deploy <dir> <domain>`) — Kubo merkleize → `store-car` (pool signer) → bind
  (owner signer). Composed and working.
- ✅ **`name register`** (open-tier commit/reveal — pulled forward from Phase 2) — this **unblocked the
  first true end-to-end deploy**: registered `trikitopenregx.dot` to alice, deployed to it, and it renders
  at **`trikitopenregx.paseo.li`** (browser-confirmed). Redeploy updates the contenthash cleanly.
- ✅ **Slice 4** (native Rust merkleization) — dropped the Kubo shell-out; `dotkit deploy` now merkleizes
  in-process via `rust-unixfs` (`FileAdder` + `BufferingTreeBuilder`; CIDv1 / raw leaves / sha2-256 / 256 KiB /
  wrap-with-directory), storing blocks straight to Bulletin (no CAR round-trip). `--kubo` keeps the old path.
  Golden-tested for byte-exact CID parity against kubo 0.40.1: pinned small-dir + a nested/multi-chunk/hidden
  fixture, plus a live cross-check on `dotshare/dist` (15 files → 19 blocks, native root == kubo root).
- ✅ **Slice 5** (config + text records) — optional `deploy.toml` (`--config` / auto-detected `./deploy.toml`,
  `deny_unknown_fields`) drives DotNS text records; `deploy` writes each `[text]` entry via dry-run-gated
  `setText` after the bind, and standalone `asset-hub name text get|set` reads/writes any record. Subname
  contenthashes deferred (need registry subnode creation).
- ✅ **`deploy --register`** (auto-register on deploy) — before merkleizing, `deploy` reads the Registry
  `owner(namehash(domain))` (ENS `owner(bytes32)`, zero = unregistered) and: proceeds if the signer already
  owns it; errors clearly if it's owned by someone else; and, when `--register` is passed, registers it
  open-tier (the proven `name register` commit/reveal flow, fused in) before uploading. Without `--register`
  an unregistered name fails fast with guidance instead of an opaque bind revert. No-op on envs without a
  registry configured (falls back to the bind dry-run). Live-verified read paths: unregistered → clear
  "not registered" error; dev-owned name → proceeds.
- ✅ **Real EVM revert reasons** — every `Revive.call` revert (`revive_view` / `revive_call` /
  `resolve_contenthash`) now decodes the returndata via `revert_reason()`: `alloy::decode_revert_reason`
  for `Error(string)`/`Panic`, an inline `SomeError(string)` decode for string-wrapped custom errors, and a
  `0x<selector> + returndata` fallback for structured custom errors. Replaced the misleading hardcoded
  "not the domain owner or the resolver is not configured" hint. Live-verified: a 4-digit `name register`
  now prints `custom error 0x2dfc7d98: "Name must have no digit suffix or exactly 2 digit suffix"` (the DotNS
  naming rule), and an unauthorized `content set` prints `custom error 0x14c417b5` with the node + caller.
- ✅ **Reliable commit/reveal (`await_commitment_mature`)** — replaced the fixed `minCommitmentAge + 6s`
  sleep with a poll: after a `min_age` floor, re-run the `register` dry-run until it stops reverting with
  `CommitmentTooNew` (selector `0x74480cc9`), bailing on any other revert, bounded by a 120s timeout. Root
  cause: the dry-run evaluates at the lagging finalized block, so a wall-clock sleep raced the on-chain clock
  and reverted ~6s short. Live-verified end-to-end: `dotshare-preview00.dot` (which previously failed with
  `CommitmentTooNew`) now registers cleanly.

**Remaining:** subname contenthashes (needs registry subnode creation), plus polish (>2 MiB single-blob
path, `preview` env addresses, register non-open tiers). Deviations from the plan below: DotNS **read**
shipped before the bind (Slice 1 was framed as bind-first); **register** was pulled forward from Phase 2
to make the MVP demoable. Slice 5 shipped **config + text records** (`deploy.toml` + `setText`/`text` +
`asset-hub name text get|set`); its optional subname piece is the one carried-over item.

# Spikes (✅ all resolved during build)

Per review, the biggest unknowns are NOT DotNS (deterministic ABI/SCALE work) — they're the
Bulletin↔gateway coupling and the chunk boundary. Prove these first; they can invalidate the slice plan.

- **Spike A — ✅ DONE (Slice 3).** DotNS bind via `subxt` + `ReviveApi.call` dry-run + signed `Revive.call`
  (`name content set`) works — namehash, `0xe301`, ABI, H160 mapping, `AssetHubConfig` all verified live.
- **Spike B — ✅ ANSWERED & BUILT (Slice 2b).** The gateway resolves a content CID by having each DAG
  block stored individually per-CID (NOT by unpacking a CAR blob). Confirmed on-chain and implemented in
  `dotkit bulletin store-car`: a fresh Kubo CAR → per-block store → the content CID + named files resolve
  on the gateway. The earlier "store CAR bytes, bind inner content CID" worry is moot — store the blocks.
- **Spike C — ✅ MOOT.** Chain cap is `MaxTransactionSize = 2 MiB` (verified). But we store per-block and
  Kubo chunks files into ≤256 KiB blocks, so no store ever approaches the boundary — no CAR-byte chunking
  needed. (A dedicated >2 MiB single-blob split is only needed for non-Kubo giant blobs; deferred.)

Only after A+B+C pass do the slices below carry low risk.

# MVP slices (ship-shaped, demoable)

Reordered per review: prove end-to-end **using Kubo as the merkleizer first**, then swap in a native
Rust merkleizer. This front-loads integration proof and isolates the UnixFS byte-fidelity swamp (the
top rework risk) to a single later slice instead of gating the first ship on it.

## Slice 0: Skeleton + chain connectivity
- **Goal**: Rust binary connects to Bulletin + Asset Hub and loads env config.
- **Scope checklist**:
  - [ ] Cargo project, top-level `clap` app `dotkit` with subcommand `deploy <dir> <domain> [--env] [--mnemonic] [--rpc] [--input-car]`; one module per surface (`bulletin`/`name`/`statement`/`account`) scaffolded even if only `deploy` is wired.
  - [ ] Port the env config subset (RPCs, para ids, DotNS + Publisher addresses) — start with `paseo-next-v2` + `preview`.
  - [ ] `subxt` clients for both chains; static codegen from pinned metadata; print runtime spec/version at startup.
  - [ ] sr25519 signer from mnemonic with derived pool sub-accounts; golden-test the derived SS58 vs the JS-derived address.
- **✅ Demo**: `deploy --env paseo-next-v2 --print-signer` prints the derived SS58 + its H160 mapping from `ReviveApi.address`.
- **Risks**: metadata drift on runtime upgrade → pin snapshots, fail loudly with a clear "run metadata update" error.

## Slice 1: Bind a known CID to a pre-owned domain (DotNS path)
- **Goal**: `deploy --set-cid <cid> <domain.dot>` binds an existing CID — the whole DotNS write path, no Bulletin.
- **Scope checklist**:
  - [ ] `node = namehash("<label>.dot")`; contenthash = `0xe301` + CID bytes.
  - [ ] ABI-encode `setContenthash(bytes32,bytes)`; `ReviveApi.call` dry-run; **use the dry-run output to set `weight_limit`/`storage_deposit_limit`** (no magic constants); submit `Revive.call`.
  - [ ] Ensure signer H160 mapping first (`Revive.map_account` if unmapped).
  - [ ] Read-back verify; wait finalized; poll the gateway/`.paseo.li` with backoff before declaring success.
- **✅ Demo**: point a pre-owned domain at a known-resolving CID; open `https://<name>.paseo.li` and see it; re-run no-ops.
- **Risks**: v1/v2 address footgun → addresses only from env config; unmapped-origin reverts look identical to missing-contract (verify H160 mapping first).

## Slice 2: Upload a Kubo CAR to Bulletin (`--input-car`)
- **Goal**: `deploy --input-car app.car <domain.dot>` uploads a pre-built CAR and (with Slice 1) binds its content CID.
- **Scope checklist**:
  - [ ] Read a CARv1, extract the root content CID from the header.
  - [ ] Split into chunks at the size Spike C validated; each → `store_with_cid_config` (raw/sha2-256).
  - [ ] Build + store the UnixFS DAG-PB file root over chunk CIDs (the storage CID).
  - [ ] Single-signer **sequential, wait-finalized** submit (dense nonces only if Spike/throughput needs it).
  - [ ] Pre-flight: check `Authorizations` remaining quota + native balance; one honest post-upload verify (recompute+compare CIDs, skip already-present).
- **✅ Demo**: generate a CAR with the existing Node tooling (or `ipfs`), upload it, fetch the content back via the gateway and diff bytes; then chain Slice 1 to bind + open `.paseo.li`.
- **Risks**: quota/fee exhaustion → surface remaining quota; gateway propagation lag → poll with backoff, don't fail fast.

## Slice 3: End-to-end deploy via Kubo merkleization (THE MVP SHIP)
- **Goal**: `deploy ./dist myapp00.dot` works end-to-end for a pre-owned domain, using Kubo to merkleize.
- **Scope checklist**:
  - [ ] Shell out to Kubo (`ipfs add -Q -r --hidden --cid-version=1 --raw-leaves --pin=false` + `dag export`) to produce the CAR + content CID. (temporary — replaced in Slice 4)
  - [ ] Glue Slice 2 (upload) + Slice 1 (bind); print CID + `.paseo.li` URL.
  - [ ] `--dump-car`.
- **✅ Demo**: deploy a real multi-file `dist/` (with `index.html`) to a pre-owned domain; open `https://<name>.paseo.li` and see the app; re-run no-ops when unchanged.
- **Risks**: requires `ipfs` on PATH (accepted for MVP; removed in Slice 4). Kubo output already meets the host contract (CIDv1/dag-pb/sha2-256), so this ships a correct deploy immediately.

## Slice 4: Native Rust merkleization (drop Kubo) — ✅ DONE
- **Goal**: replace the Kubo shell-out with in-process Rust UnixFS so the tool is a single dependency-free binary.
- **Approach (de-risked)**: use `rust-unixfs` 0.6.0 (`FileAdder` for files → `(Cid, block)` pairs; `BufferingTreeBuilder`
  for the dir DAG-PB root). Its defaults already match `ipfs add -r --cid-version=1
  --raw-leaves` (CIDv1, raw leaves, sha2-256, 256 KiB chunks, balanced bf=174), so this was "wire + verify", not "hand-roll UnixFS".
- **Scope checklist**:
  - [x] `FileAdder::builder().with_cid_version(V1)` per file (V1 default = raw leaves); collect blocks.
  - [x] `BufferingTreeBuilder` (wrap-with-directory) → root dir CID; `index.html` (and all files) linked by relative path.
  - [x] Root found via dag-pb link analysis; blocks stored **straight to Bulletin** (skipped the CAR round-trip entirely — no `iroh-car` write needed on the native path).
  - [x] `rust-unixfs` pinned via `Cargo.lock` (0.6.0); Kubo path kept as `--kubo` fallback.
- **✅ Demo**: golden tests assert the Rust content CID **equals** the Kubo CID — a pinned small-dir vector, a nested + >256 KiB multi-chunk + hidden-file fixture, and a live cross-check on `dotshare/dist` (15 files → 19 blocks). Merkleization runs with no `ipfs` binary present.
- **Note**: only the on-chain live deploy with the native encoder is left to eyeball on `.paseo.li` (needs a real chain write). Symlinks in the build dir are followed as files (not encoded as UnixFS symlinks) — a known, low-impact divergence for typical `dist/` output.

## Slice 5: Config file + text records — ✅ DONE (config + text; subnames deferred)
- **Goal**: Match the existing Node tooling's config-driven metadata writes.
- **Scope checklist**:
  - [x] Read an existing deploy config file — Rust-native `deploy.toml` (`--config <path>` or auto-detected `./deploy.toml`; `serde`/`toml` with `deny_unknown_fields` so typos/unsupported sections fail loudly). Loaded up front so a bad config fails before any chain/merkleize work.
  - [x] Write `manifest`/`executable` (and any) text records via `setText(bytes32,string,string)` — new `dotns::{encode_text_call,decode_text_return,encode_set_text_call}` + `chain::{resolve_text,set_text}` (dry-run-gated `Revive.call`, read-back verified). `deploy` writes every `[text]` record after the bind; standalone `asset-hub name text get|set` added.
  - [ ] Optional `app|widget|worker.<domain>` subname contenthashes — **deferred**: needs parent-owner subnode creation on the registry (ABI not in scope / unverified), so a bare `setContenthash` on a subname node would just revert. Left out rather than ship a reverting feature.
- **✅ Demo**: `deploy ./dist myapp00.dot --config deploy.toml` writes the records; `dotkit asset-hub name text get myapp00 manifest` returns the written value. (Offline-verified: config parse/rejection + CLI surface + build/tests green; the live on-chain set/get is the only bit left to eyeball, same as Slice 4's native deploy.)

# Contracts (guardrails — must not regress)
- `--env` selects Bulletin RPC **and** Asset Hub contract addresses as a matched set.
- Content CID bound to DotNS must be fetchable via `https://<cid>.app.paseo.li` and `https://<name>.paseo.li`.
- **Content root must be CIDv1 / dag-pb (or raw single-file) / sha2-256, CARv1 single root, with `index.html`** — the web host fails closed on any other multihash/codec (Host compatibility contract).
- Any preimage/icon blob resolved via the host preimage API must be CIDv1 / raw / blake2b-256.
- Never write a contract call without a successful `ReviveApi.call` dry-run first, and set
  `weight_limit`/`storage_deposit_limit` from that dry-run — never magic constants.
- Wait for **finalized** on Bulletin chunks/root and the DotNS write; then poll the gateway with backoff
  before declaring success or failure (propagation lag is expected, not an error).
- Never submit a Revive write before confirming the signer's SS58↔H160 mapping.
- Secrets (`--mnemonic`) only via env var by default; never logged.

# Key decisions (decide early)
- **subxt static codegen vs dynamic**: static (pinned metadata) for stable pallets (`TransactionStorage`,
  runtime APIs); **dynamic metadata for the `pallet_revive` path** (it's experimental and shifts call
  indices between upgrades). Print runtime spec/version at startup; fail loudly on unknown revive version.
- **UnixFS/CAR crate**: `rust-unixfs` 0.6.0 is the only live Rust UnixFS encoder and is single-maintainer;
  ✅ proven vs Kubo 0.40.1 by our golden suite (pinned + live cross-checks), pinned via `Cargo.lock`, with `--kubo` as a fallback.
- **CID fidelity bar**: root MUST be CIDv1 / dag-pb / **sha2-256** with `index.html` (the web host verifier is the binding constraint; the desktop + mobile hosts are looser). "resolvable on the gateway" for MVP; "bit-identical to Kubo" is a later, separate goal (sha2-256 dag-pb already matches Kubo's default, so they converge).
- **Signer source for MVP**: reuse the existing authorized pool derivation so no `bulletin-bootstrap` is needed.
- **ABI stack**: `alloy` for encode/decode/selectors + keccak; implement namehash manually.

# Testing
- Manual smoke demo per slice (CIDs proven through the real gateway, not our own encoder).
- Contract regression tests: namehash vectors, `0xe301` contenthash encoding, `setContenthash` calldata
  golden bytes vs a viem reference, sr25519 derived-pool address vs the JS-derived address.
- **UnixFS golden fixtures (Slice 4 gate)**: Rust content CID must equal Kubo's for a fixture matrix —
  tiny dir, nested dirs, a file larger than one chunk, non-ASCII filename, single-file dir. No parity → keep Kubo.

# Polish phases (after MVP)

## Phase 1: Robustness + parity
- Retry/backoff knobs, reconnection, better nonce recovery, larger-deploy heap-free streaming.
- ✅ Check-in demo: deploy a ~20 MB site reliably end-to-end.

## Phase 2: Naming lifecycle
- Domain registration: commit → wait `minCommitmentAge` (+buffer) → `register`, PoP tier reads,
  pricing, transfer, `Publisher.publish` (paseo-next-v2).
- ✅ Check-in demo: register a fresh open-tier name from zero and deploy to it.

## Phase 3: Reproducibility + extras
- Deterministic timestamps → bit-identical CAR; encryption (`--password`); `--input-car`; gh-pages mirror.
- ✅ Check-in demo: two runs on the same input produce identical CARs/CIDs.

# Later / Deferred
- **`dotkit statement` (People / Statement Store)** — publish/subscribe/submit. Research-first: verify the
  People-chain submission path (pallet vs statement-store RPC, SCALE shapes, signing) before designing. The
  host-bridged statement-store client covers the host path; a standalone CLI needs the local/BYOD path.
- **`dotkit name register`** — commit/reveal + PoP-gated registration, transfer, Publisher — promote from Phase 2.
- Pool bootstrap CLI (`authorize_account` as an authorizer) — revisit when running a fresh testnet.
- Mobile/host-session allowance path — revisit only if the tool must run inside a host session.
- Mainnet (Polkadot/Kusama) Bulletin — blocked until Bulletin ships there.
