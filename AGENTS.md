# dotkit

Single-binary Rust CLI for the Polkadot Triangle ecosystem: Bulletin storage +
DotNS naming on Asset Hub (`pallet_revive`). Design source of truth and shipped
status: `docs/plans/rust-chain-deploy-tool.md`.

## Build & verify

- `cargo build`, `cargo test`, `cargo clippy --all-targets` (keep it warning-clean).
- dotkit talks to **live testnets** — verify behavior against the live chain + IPFS
  gateway, not assumptions. Prefer read-only `ReviveApi.call` dry-runs and gateway
  round-trips over guessing. The plan doc records "facts" from the JS tools this
  replaces that turned out wrong (e.g. the 8 MiB cap that was really 2 MiB).

## Chain gotchas

- `--env` selects the Bulletin RPC **and** the Asset Hub contract addresses as one
  matched set (`src/env.rs`); never mix v1/v2. Full registrar/registry addresses
  exist only for `paseo-next-v2`; `preview` is content-resolver-only.
- **DotNS registration rules** — the label digit-suffix rule and the commit/reveal
  `CommitmentTooNew` timing — are documented in the `src/registrar.rs` module docs.
  Read those before touching the register / deploy flow.
- All `Revive.call` reverts decode real returndata via `chain::revert_reason` —
  surface the actual on-chain error, don't hardcode "probably X" hints.
- `pallet_revive` writes require an SS58↔H160 mapping and a successful dry-run;
  derive weight / storage-deposit limits from the dry-run, never magic constants.

## Skill (keep in sync)

- Agent-facing doc is `skills/dotkit/SKILL.md` (single source of truth).
- Update it in the **same change** when the command/flag surface, `--env` set, signer
  model, naming/PoP rules, or revert wording changes. Match `dotkit --help`.
