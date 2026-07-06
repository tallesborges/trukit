# AGENTS.md

This file gives coding agents the context they need to work in this repository.

## Project

`dotkit` — a single-binary Rust CLI for the Polkadot Triangle ecosystem: Bulletin
storage + DotNS naming on Asset Hub (`pallet_revive`). The first-class command is
`dotkit deploy` (merkleize a build dir → upload to Bulletin → bind the `.dot` contenthash).

## Build & verify

- `just check` is the pre-commit gate: `cargo fmt` + `cargo clippy --all-targets` + `cargo test`. Keep it warning-clean.
- dotkit talks to **live testnets** — verify behavior against the live chain + IPFS
  gateway, not assumptions. Prefer read-only `ReviveApi.call` dry-runs and gateway
  round-trips over guessing.

## Chain gotchas

- **Pinned metadata.** `chain/config.rs` static-codegens from `artifacts/paseo_next_v2_{asset_hub,bulletin}.scale`.
  If a call breaks after a runtime upgrade (subxt reports stale metadata), regenerate
  with `just metadata` — don't hand-edit the `.scale` files.
- **`--env` is a matched set** — it selects the Bulletin RPC **and** the Asset Hub
  contract addresses together (`src/env.rs`); never mix v1/v2. Full registrar/registry
  addresses exist only for `paseo-next-v2`; `preview` is content-resolver-only.
- **DotNS registration rules** — the label digit-suffix rule, the commit/reveal
  `CommitmentTooNew` timing, and the personhood (PoP) tiers — live in the
  `src/dotns/registrar_abi.rs` module docs (the register/deploy flow itself is in
  `src/dotns/names.rs`). Read them before touching the register / deploy flow.
- **Surface real reverts.** All `Revive.call` reverts decode returndata via
  `chain::revive::revert_reason`; show the actual on-chain error, don't hardcode "probably X" hints.
- **`pallet_revive` writes** need an SS58↔H160 mapping and a successful dry-run first;
  derive weight / storage-deposit limits from the dry-run, never magic constants.

## Live-write commands (don't run to "test")

The signed `just` recipes (`deploy`, `register`, `set`, `store`) and their `dotkit`
subcommands submit **real transactions to paseo-next-v2** — they register actual `.dot`
names, spend testnet funds, and write to Bulletin. Don't run them just to check the build;
use `cargo test` or the read-only recipes (`just whoami | env | resolve | status`). The
default signer is the public dev phrase (its `//Alice` / `//deploy/N` derivations are funded on paseo-next-v2).

## Skill (keep in sync)

- The agent-facing usage doc is `skills/dotkit/SKILL.md` (single source of truth).
- Update it in the **same change** when the command/flag surface, `--env` set, signer
  model, naming/PoP rules, or revert wording changes. Match `dotkit --help`.
