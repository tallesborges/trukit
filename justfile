# dotkit — a CLI for the Triangle/Trinity ecosystem (Bulletin · DotNS · People)
# Run `just` (or `just --list`) to see all recipes.

# The well-known Substrate dev phrase (PUBLIC — not a secret). Its //Alice,
# //Bob and //deploy/N derivations are funded / Bulletin-authorized on the
# paseo-next-v2 testnet. Override any recipe's account with a different path.
dev_phrase := "bottom drive obey lake curtain smoke basket hold race lonely fit walk"

# Default: list recipes
default:
    @just --list

# ── Build / install ──────────────────────────────────────────────────────────

# Debug build
build:
    cargo build

# Optimized release build
release:
    cargo build --release

# Install to ~/.cargo/bin (rerun after code changes)
install:
    cargo install --path . --force

# Run tests
test:
    cargo test

# Format
fmt:
    cargo fmt

# Lint
clippy:
    cargo clippy --all-targets

# Format-check + clippy + tests (pre-commit gate)
check: fmt clippy test

# Run the CLI against the current source, e.g. `just run account env`
run *args:
    cargo run --quiet -- {{args}}

# ── Read-only shortcuts (installed binary; no signing) ───────────────────────

# Signer identity: SS58 + H160 mapping (defaults to dev //Alice)
whoami:
    dotkit account whoami

# Resolved environment config
env:
    dotkit account env

# Resolve a .dot name to its content CID, e.g. `just resolve host-playground.dot`
resolve name:
    dotkit asset-hub name resolve {{name}}

# Bulletin upload quota for the (random) pool signer
status:
    dotkit bulletin status

# ── Signed shortcuts (testnet dev accounts) ──────────────────────────────────

# Deploy a folder to a domain you own. Usage: `just deploy ./dist myname.dot [//Alice]`
deploy dir domain path="//Alice":
    MNEMONIC="{{dev_phrase}}" dotkit deploy {{dir}} {{domain}} --derivation-path {{path}}

# Register an open-tier name to a dev account. Usage: `just register myname.dot [//Alice]`
register name path="//Alice":
    MNEMONIC="{{dev_phrase}}" dotkit asset-hub name register {{name}} --derivation-path {{path}}

# Point an owned name at an existing CID. Usage: `just set myname.dot <cid> [//Alice]`
set name cid path="//Alice":
    MNEMONIC="{{dev_phrase}}" dotkit asset-hub name content set {{name}} {{cid}} --derivation-path {{path}}

# Store a single file on Bulletin. Usage: `just store ./file.bin`
store file:
    dotkit bulletin store {{file}}

# ── Maintenance ──────────────────────────────────────────────────────────────

# Regenerate pinned chain metadata (run after a runtime upgrade breaks calls)
metadata:
    subxt metadata --url wss://paseo-asset-hub-next-rpc.polkadot.io -f bytes > artifacts/paseo_next_v2_asset_hub.scale
    subxt metadata --url wss://paseo-bulletin-next-rpc.polkadot.io  -f bytes > artifacts/paseo_next_v2_bulletin.scale

# Remove build artifacts
clean:
    cargo clean
