#!/usr/bin/env bash
set -euo pipefail

# tongs setup script
# Idempotent - safe to run multiple times

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== tongs Setup ==="

check_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "ERROR: $1 is required but not installed (see https://rustup.rs)."
        exit 1
    fi
}

check_command cargo
check_command rustc

if ! cargo fmt --version >/dev/null 2>&1; then
    echo "ERROR: rustfmt is required. Install it with: rustup component add rustfmt"
    exit 1
fi

cd "$PROJECT_DIR"

echo "Building..."
cargo build --locked

echo "Running tests..."
cargo test --locked

echo "=== Setup complete ==="

echo "Running verification..."
cargo run --quiet --locked -- --version
echo "OK: tongs builds and the test suite passes."
echo "Install the CLI with: cargo install --locked --path ."
