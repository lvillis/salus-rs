set shell := ["bash", "-euo", "pipefail", "-c"]

ci:
    cargo fmt --all --check
    cargo check --all-features --locked
    cargo check --no-default-features --locked
    cargo clippy --all-targets --all-features --locked -- -D warnings
    cargo clippy --all-targets --no-default-features --locked -- -D warnings
    cargo test --all-features --locked
    cargo test --no-default-features --locked

patch:
    cargo release patch --no-publish --execute
