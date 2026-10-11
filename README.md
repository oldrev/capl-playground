# CAPL Playground

CAPL Playground is a Rust-first, local-friendly CAPL and CAN simulation workbench. The compiler remains a reusable Cargo workspace; the desktop shell uses Longbridge's GPUI Kit and provides a VS Code-like dock layout.

The useful path is recorded traffic → virtual time → CAPL event handlers:

```text
ASC / candump ─▶ trace model ─▶ virtual-time replay ─▶ on message handlers
DBC / LDF ────▶ database model ─▶ signal decoding ───▶ tables and plots
CAPL source ──▶ lexer/parser/sema ─▶ WASM module ─────▶ the same runtime
```

## Rust workspace

```text
rust/crates/capl-parse          CAPL lexer and parser
rust/crates/capl-sema           semantic analysis
rust/crates/capl-driver         compiler orchestration and CLI
rust/crates/capl-codegen-*      C and WebAssembly backends
rust/crates/capl-model          format-independent CAN/LIN models
rust/crates/capl-trace-core     trace events and reader interface
rust/crates/capl-trace-asc      Vector ASC and candump text reader
rust/crates/capl-database-core  messages, signals, nodes and decoding
rust/crates/capl-database-dbc   DBC reader
rust/crates/capl-project        .caplproj JSON manifest and storage trait
rust/crates/capl-runtime        virtual clock, replay and native WASM host
rust/crates/capl-sysvars        namespaced system-variable registry and events
rust/crates/capl-desktop        GPUI Kit desktop application
```

New formats should add a crate under the appropriate family (`capl-trace-*` or `capl-database-*`) and emit the shared model. Parsers do not depend on the UI or runtime. LDF, BLF, MF4 and ARXML can be added without changing the simulator.

## Build and test

Run from `rust/`:

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p capl-desktop
cargo run -p capl-driver -- path/to/file.can --emit=wat
cargo run -p capl-replay -- path/to/node.can path/to/traffic.asc

# Browser/WASM GUI (PowerShell)
./rust/serve-web.ps1
```

The native shell starts with `cargo run -p capl-desktop`. The browser shell is
served by Trunk; install it once with `cargo install --locked trunk`.
GPUI's current WebAssembly backend requires nightly, so install the target once
with `rustup +nightly target add wasm32-unknown-unknown`; `serve-web.ps1` sets
the toolchain automatically.

The fixed project extension is `.caplproj`. Its JSON manifest can be stored by the native filesystem adapter or by a browser adapter backed by local storage.

## Replay

`capl-trace-asc` accepts common Vector ASC and candump text lines. The runtime keeps a virtual timestamp, matches frames by CAN id and channel, writes the CAPL message object into WASM memory, and invokes matching `on message` exports. Replay can therefore sanity-check a script against recorded traffic without connecting to a vehicle.

Binary trace formats and complete DBC/LDF coverage are intentionally separate follow-up crates. The current DBC reader supports the basic message, node and signal model used by replay and decoding.

## Disclaimer

CAPL Playground is an independent compatibility and teaching project. It is not sponsored, endorsed, affiliated with, or derived from Vector Informatik GmbH or its products.

## License

MIT OR Apache-2.0.
