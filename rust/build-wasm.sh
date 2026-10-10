#!/usr/bin/env bash
# Build the Rust compiler core and drop it where the playground can find it.
#
#   ./rust/build-wasm.sh            # release build into ../public/capl
#   ./rust/build-wasm.sh --dev      # debug build, keeps symbols
#
# Requirements: rustup target add wasm32-unknown-unknown && cargo install wasm-pack
set -euo pipefail

cd "$(dirname "$0")"

PROFILE="--release"
[[ "${1:-}" == "--dev" ]] && PROFILE="--dev"

echo "▸ cargo test (all crates, native)"
cargo test --workspace --quiet

echo "▸ cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "▸ wasm-pack build capl-wasm"
wasm-pack build crates/capl-wasm \
  $PROFILE \
  --target web \
  --out-name capl_wasm \
  --out-dir ../../../public/capl

echo "▸ native CLI"
cargo build --release -p capl-driver --bin capl
echo "  → target/release/capl"

cat <<'EOF'

Done. The playground's index page loads the generated Rust bundle from
public/capl/capl_wasm.js before React starts. TypeScript remains the UI layer;
there is no compiler fallback, and a missing Rust bundle is reported as a load
failure.
EOF
