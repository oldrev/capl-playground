# AGENTS.md

## Project Overview

CAPL Playground is a browser-based playground for Vector CAPL. The React and
Vite frontend loads a Rust compiler compiled to WebAssembly. The Rust pipeline
is organized as reusable crates for diagnostics, AST construction, parsing,
semantic analysis, code generation, driver orchestration, and the WASM facade.

The repository has two main areas:

- `src/`: React, TypeScript, and Tailwind frontend code.
- `rust/`: the Cargo workspace containing the compiler and WASM crates.

The lexer is part of the `capl-parse` crate as the `lexer` module. Keep lexer
changes there; do not reintroduce a separate `capl-lexer` workspace member
unless the project develops a real independent consumer and stable public API.

## Working Conventions

- Read the surrounding module and its tests before changing an interface.
- Prefer existing crate boundaries, traits, and helper APIs over new
  abstractions.
- Keep parser code syntax-only. Name resolution, type checking, constant
  folding, and other semantic behavior belong in `capl-sema`.
- Keep AST nodes as data. Backends should consume the semantically decorated
  AST without mutating it.
- Preserve source locations and diagnostics when changing compiler stages.
- Keep frontend changes compatible with the `globalThis.__CAPL_CORE__` WASM
  contract and the simulated runtime in `src/runtime/`.
- Update relevant README documentation when repository structure or developer
  commands change.

## Rust Commands

Run Rust commands from `rust/`:

```bash
cargo check --workspace
cargo test --workspace
cargo run -p capl-driver -- path/to/file.can --emit=wat
```

For coverage, use the commands documented in `rust/Cargo.toml` and the
repository README. The driver integration tests in
`rust/crates/capl-driver/tests/` exercise complete compiler pipelines and
should be updated when user-visible CAPL behavior changes.

## Frontend Commands

Run frontend commands from the repository root:

```bash
npm run dev
npm run build
npm run preview
```

Use the repository's existing package manager and lockfile. Avoid unrelated
dependency upgrades or generated-file churn.

## WASM Build

The browser compiler is generated through the Rust WASM build script:

```bash
./rust/build-wasm.sh
```

When changing the Rust-to-TypeScript contract, rebuild the WASM artifact and
verify the browser-facing code still handles diagnostics, AST JSON, generated
outputs, and runtime execution.

## Validation Expectations

For Rust changes, run at least `cargo check --workspace` and
`cargo test --workspace`, then run `cargo clippy --workspace --all-targets
--all-features -- -D warnings` after the implementation is complete. For
frontend changes, run `npm run build`. For cross-layer changes, run both sets
of checks and rebuild WASM when applicable.

Do not hide compiler errors by weakening diagnostics or broadly relaxing type
checks. Add focused tests for parser, semantic, backend, or runtime behavior
when the change affects a meaningful user-facing path.
