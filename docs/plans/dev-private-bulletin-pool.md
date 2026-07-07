# dotkit — per-machine private Bulletin pool for dev (Ship-First Plan)

Today every dotkit user on testnet uploads Bulletin blocks through the **same** shared
`DEV_PHRASE//deploy/{0..9}` pool (`chain::pool_signer()`), contending on nonces and quota with
the official `bulletin-deploy` pool and everyone else. This plan replaces that — **in dev only** —
with a **per-machine private pool** derived from a locally-generated mnemonic, authorized once via
`//Alice` in a single batch. The shared pool is **demoted to an opt-in fallback**, never removed.

> Scope: `paseo-next-v2` / dev only. Default owner/bind signer and DotNS flows are unchanged.

## Motivation

- **Isolation** — you only contend on nonces/quota with yourself, not with the whole world.
- **No shared-quota exhaustion** — others can't drain the pool you rely on.
- **Extensible** — grow your pool (`--accounts N`) whenever you want more upload concurrency.

## Grounding (verified against the code + docs, 2026-07)

- `chain::pool_signer()` picks a random `DEV_PHRASE//deploy/{0..9}`; only those derivations hold
  Bulletin quota. `chain::build_signer()` defaults to the base `DEV_PHRASE`.
- **Authorizer key is `//Alice`** off the dev phrase (`docs/plans/rust-chain-deploy-tool.md`,
  `AGENTS.md`). So dotkit can already sign `TransactionStorage.authorize_account` — no missing key.
  `//Alice` and `//deploy/N` are **siblings** off the same root phrase, not parent/child.
- **Bulletin writes are quota-gated, not fee-gated.** The existing `//deploy/N` accounts aren't
  specially Asset-Hub funded and still upload. So freshly derived accounts need Bulletin
  **authorization only** — no PAS funding — to upload.
- **Upload pool vs owner signer are independent.** Bulletin is content-addressed: the same files
  always produce the same CID regardless of who uploads, and already-present blocks are skipped.
  What gates an upgrade/redeploy is the **DotNS name owner** (the `owner` signer that re-binds the
  contenthash), already overridable via `--mnemonic` / `--derivation-path`. Swapping the upload pool
  changes nothing about upgradeability.

## Design

- **Local keystore** at `~/.dotkit/pool.toml`, mode `0600`, with a testnet-only banner. Holds a
  randomly-generated mnemonic + the account count.
- **Private pool** = `<local-mnemonic>//deploy/{0..N}` (default N=10), same derivation shape as the
  shared pool but off your own root secret.
- **One-time bootstrap** authorizes all N via `//Alice` in a single `utility.batch_all`
  (one signature). Idempotent: read `TransactionStorage.Authorizations` first, skip already-authorized.
- **`pool_signer()` becomes env-aware**: use the local pool when the keystore exists, else fall back to
  the shared `DEV_PHRASE//deploy/N` pool.
- **Opt-in override** via `--pool local|shared` (`--pool shared` forces the official pool for a run).
- The shared derivations **stay in code** as the fallback — demoted, not deleted.

## Phases (each demoable)

### Phase 0 — De-risk spike (read/prove, no code) ✅ DONE (2026-07-07)
Prove on `paseo-next-v2` with existing commands only:
- `//Alice` can `bulletin authorize` a throwaway derived account. ✅ tx `0x4929bfe9…5453` (1000 txs / 10 MiB granted).
- That account can then `bulletin store` a blob with **zero Asset Hub balance**. ✅ throwaway `5EZZ…Cu9Vw` (0 PAS) stored 46 B at block `901925`; read-back shows `authorized:true`, `transactions 1/1000`, `bytes 46/10485760`.

**Verdict:** both load-bearing assumptions confirmed — Authorizer key is available (`//Alice`) and Bulletin uploads need no Asset Hub funding (quota-gated). Green-lights everything below.

### Phase 1 — Local keystore ✅ DONE (2026-07-07)
- `dotkit bulletin pool init`: generate a random mnemonic, persist to `~/.dotkit/pool.toml` (`0600`).
- Derive `//deploy/{0..N}` off the local mnemonic; print the accounts. No chain writes yet.
- Shipped: `src/pool.rs` (keystore) + `bulletin pool init [--accounts N] [--force]` / `bulletin pool status`. Verified: `0600` perms, deterministic derivation, `--force` regen, `--json`, `--accounts 0` guard, existing-keystore guard. Docs synced (README + SKILL). (Commands live under `bulletin pool`, moved from the earlier `dev pool`.)

### Phase 2 — Batch authorize ✅ DONE (2026-07-07)
- Single `utility.batch_all` of `authorize_account(who, txs, bytes)` ×N, signed by `//Alice`.
- Idempotent (skip already-authorized via `Authorizations`).
- Shipped: `bulletin::batch_authorize_accounts` + `bulletin::is_authorized` (`src/bulletin/storage.rs`) and `dotkit dev pool authorize [--transactions N] [--bytes N]` (`src/commands/dev.rs`). Verified on `paseo-next-v2`: authorized all 10 accounts in one tx `0xb86a…510f`; on-chain read-back shows `authorized:true` (1000 txs / 10 MiB); re-run skipped all 10 with no tx. Docs synced.
- **Demo:** `init` → all N show authorized in `bulletin status`.

### Phase 3 — Wire pool_signer + opt-in flag ✅ DONE (2026-07-07)
- `pool_signer()` prefers the local keystore; falls back to the shared pool.
- Add `--pool local|shared` (default: local if keystore exists, else shared).
- Owner signer stays independent (`--mnemonic`/`--derivation-path`), so "redeploy as the account that
  owns this name" already works.
- Shipped: `dev::PoolSource` + `dev::pool_signer(source)` (auto/local/shared, with a one-line pool/account note); renamed `chain::pool_signer` → `chain::shared_pool_signer`; global `--pool` flag; `deploy` + `bulletin store/store-car/status` route through it. Verified on `paseo-next-v2`: default→`private //deploy/1` (0/1000 txs), `--pool shared`→congested shared account (1031/1000 txs, 412 MB/100 MB), `--pool local`→clean; real `bulletin store` uploaded via `private //deploy/7` (block #902475). Docs synced.

### Phase 4 — Polish
- `--accounts N` to grow the pool; `dotkit bulletin pool status`.
- Sync `README.md` + `skills/dotkit/SKILL.md` in the **same change** (required by `AGENTS.md`).

## Costs / risks

- One-time bootstrap batch per machine.
- A plaintext **testnet-only** dev key on disk (`0600`, low risk — no mainnet value).
- Behavior on `paseo-next-v2`/dev only; the official pool remains the fallback everywhere else.

## Open item

- ~~Phase 0 must confirm on-chain that `//Alice` still holds Authorizer rights on `paseo-next-v2` and that an authorized-but-unfunded account can store.~~ **✅ Confirmed 2026-07-07** (see Phase 0). Ready to build Phase 1.
