# Run inside the reproducible development shell: nix develop --command just ci

default:
    @just --list

ci: ci-rust ci-frontend ci-tools

ci-rust:
    cargo fmt --all -- --check
    cargo check --locked -p bmc-backend-model --no-default-features
    cargo check --locked -p bmc-idrac-model --no-default-features
    # Keep the documented async-trait and Axum compatibility exceptions.
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -A clippy::double_must_use -A clippy::result_large_err
    cargo test --locked --workspace --all-targets --all-features

ci-frontend: frontend-install
    pnpm --dir frontend lint
    pnpm --dir frontend test
    pnpm --dir frontend build

ci-tools:
    python3 scripts/generate_feature_contracts.py --check
    python3 -m unittest discover -s scripts -p 'test_*.py'

generate-contracts:
    python3 scripts/generate_feature_contracts.py

frontend-install:
    pnpm --dir frontend install --frozen-lockfile

build-frontend: frontend-install
    pnpm --dir frontend build

build: build-frontend
    cargo build --locked --release

fmt:
    cargo fmt --all

verify-idrac device_id:
    python3 scripts/verify_idrac_device.py {{quote(device_id)}}
