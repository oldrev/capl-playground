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
cargo clippy --workspace --all-targets -- -D warnings || true

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

Done. The playground picks the Rust core up automatically: add

  <script type="module">
    import init from "/capl/capl_wasm.js";
    await init();            // registers globalThis.__CAPL_CORE__
  </script>

to index.html before the app bundle. Without it the page falls back to the
TypeScript mirror in src/core/, which implements the exact same pipeline.
EOF
