# CAPL Playground

An online playground for **Vector CAPL** (the CANoe/CANalyzer node language).
Write CAPL in the browser, watch it go through a real compiler pipeline, inspect
the AST as JSON, read the generated C, read the generated WebAssembly — and then
*run* that WebAssembly inside a miniature simulated measurement with virtual
time, timers, a CAN bus and key events.

Nothing is sent to a server: the compiler runs in the page.

```
source.can ─▶ lexer ─▶ parser ─▶ AST ─▶ sema ─┬─▶ wasm backend ─▶ .wasm ─▶ simulator
                                              ├─▶ C backend    ─▶ .c
                                              └─▶ AST backend  ─▶ .json
```

## Why it is shaped like DMD

The front end copies the layout that makes DMD's source tree so easy to read:

| stage | crate | rule it obeys |
|---|---|---|
| scanning | `capl-lexer` | a hand-written state machine, no regex, keeps trivia so an editor can reuse it |
| parsing | `capl-parse` | **pure syntax**: never resolves a name, never folds a constant, never reports a type error |
| tree | `capl-ast` | nodes are data; every expression has an empty `ty` slot |
| semantics | `capl-sema` | two passes (declarations, then bodies) that *decorate the tree in place* |
| targets | `capl-codegen-*` | read-only consumers of the decorated tree, chosen at run time through one trait |

Because the stages only talk through the AST, every crate is independently
useful: take `capl-lexer` for an editor, `capl-parse` for a linter,
`capl-sema` for a static checker, or the whole `capl-driver` for a compiler.

## Repository layout

```
src/                      the playground UI (React + Vite + Tailwind v4)
  core/                   Rust WASM bridge and editor-only syntax lexer
    driver.ts             frontend contract for capl-wasm
  runtime/sim.ts          the simulated CANoe measurement (host imports)
  components/             editor, panels, architecture tab
rust/                     the compiler core as reusable crates
  crates/capl-diag        spans + diagnostic bag
  crates/capl-lexer       scanner
  crates/capl-ast         AST, type lattice, visitor, serde JSON export
  crates/capl-parse       recursive-descent parser
  crates/capl-sema        scopes, symbols, types
  crates/capl-codegen-wasm  wasm module writer (LEB128 + sections, no deps)
  crates/capl-codegen-c     C99 writer
  crates/capl-driver      stage orchestration, Backend trait, `capl` CLI
  crates/capl-wasm        wasm-bindgen facade for this page
```

### Rust compiler, TypeScript UI

The page loads `capl-wasm` before React starts. It registers the sole compiler
implementation on `globalThis.__CAPL_CORE__`; TypeScript only owns the editor,
simulation host and presentation layer.

```bash
./rust/build-wasm.sh          # wasm-pack build → public/capl/capl_wasm.js
cargo run -p capl-driver -- node.can --emit=wat
cargo run -p capl-driver -- node.can --emit=wasm -o node.wasm
cargo test --workspace
```

### Rust test coverage and CAPL fixtures

The compiler has driver-level integration tests under
`rust/crates/capl-driver/tests/`. They compile real CAPL programs through the
Rust lexer, parser, semantic pass, C backend, and WebAssembly backend. The
fixtures cover timers and handlers, cyclic CAN messages, arrays and functions,
64-bit and floating-point expressions, plus invalid input diagnostics.

Install [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) once, then
run:

```bash
cd rust
cargo llvm-cov --workspace --all-features --lcov --output-path coverage/lcov.info
cargo llvm-cov --workspace --all-features --html --open
```

On Windows, `./rust/coverage.ps1` runs the LCOV command and prints the summary.
CI installs the same tool and produces `rust/coverage/lcov.info` during the
Rust test job, then uploads it as the `rust-coverage` artifact. The native
coverage command intentionally measures the reusable compiler crates; the
`capl-wasm` facade is exercised by the browser build and runtime smoke tests.

## The generated module

The wasm backend writes the binary by hand — LEB128, sections, a code builder
that emits bytes and WAT listing in lock-step.

```
0x0000  null guard
0x0040  vararg marshalling area (32 slots × 16 B) used by write()
0x0400  string pool, globals, message objects, function statics
```

Imports (module `"capl"`, supplied by the host — the browser simulator here, a
CANoe shim or a test bench elsewhere):

```
write(fmt, argc, args)   setTimer(id, ms)   setTimerCyclic(id, ms)
cancelTimer(id)          output(msgPtr)     timeNow() -> i32
random(n) -> i32         strlen(ptr) -> i32 pow/sin/cos
canOnline()              canOffline()       stopMeasurement()
```

Exports: `memory`, the immutable global `__msgbuf` (address of the event
message object), `__init`, `__set_this`, `__set_fuel`, every user function and
one function per event handler (`on_start`, `on_timer_tick`,
`on_message_0x100`, `on_key_103`, …).

Every loop back-edge decrements a fuel counter and traps when it hits zero, so
an accidental `while (1) {}` cannot freeze the tab — the simulator reports it as
a runtime trap instead.

## Supported language subset

`variables{}` blocks (which may also define types) · globals · `const` · arrays
and brace initialisers · `byte word dword qword int long int64 float double char`
· **`enum`** with implicit and explicit values, usable directly in `switch`
labels · **`struct`** with computed member layout — offsets, natural alignment
and trailing padding are worked out by `capl-sema` and carried *inside the type*,
so no backend needs a symbol table · **multi-dimensional arrays**, arrays of
structs and struct members of array type · message objects with
`.id .dlc .can .byte(n) .word(n) .dword(n)` and `this` · `timer`/`msTimer` ·
events `preStart start stopMeasurement preStop timer message key sysvar envVar
errorFrame busOff` (with channel-aware matching for `on message CAN2.0x100`) ·
`if else while do for switch` (with fall-through) `break continue return` · the
full C expression grammar including `?:`, short-circuit `&&`/`||` and compound
assignment · user functions with recursion and array parameters · built-ins
`write` (printf-style), `output`, `elCount`, `timeNow`, `random`, `abs`, `min`,
`max`, `sqrt`, `pow`, `sin`, `cos`, `floor`, `ceil`, `strlen`, `canOnline`,
`canOffline`, `stopMeasurement`.

Local aggregates are cleared on declaration by the generated `__zero` helper,
which keeps examples reproducible. Compile-time constants (enum members,
`const int N = 3`) are folded into immediates and never occupy linear memory.

Not implemented: `includes{}`, database signal access (`$Signal`), test modules,
passing or returning aggregates by value, and whole-struct assignment (the
diagnostic tells you to copy member by member).

## UI

* light / dark / **system** theme, remembered in `localStorage`
* English / 简体中文, auto-detected from `navigator.language`
* diagnostics with precise spans, clickable, mirrored in the editor gutter
* per-stage timings, token/node counts and module size in the status bar
* simulation bar: virtual clock, `+100 ms` / `+1 s`, frame injection, key events,
  pending-timer inspector

Keyboard: `Ctrl`/`⌘` + `Enter` builds and runs.

## Disclaimer

CAPL Playground is an independent, experimental compatibility and teaching
project. It is not sponsored, endorsed, affiliated with, or derived from
Vector Informatik GmbH, CANoe, CANalyzer, or any other Vector product. “Vector”,
“CAPL”, “CANoe”, and “CANalyzer” are names and trademarks of their respective
owners and are used here only to identify the language or behavior this project
aims to interoperate with. This project does not include Vector software,
proprietary databases, documentation, or test assets, and it does not claim to
implement the official CAPL toolchain.

Use of this project is at your own risk. You are responsible for checking the
licenses and terms that apply to any source code, specifications, examples,
trademarks, reverse-engineering activity, and deployment environment involved
in your use of it. This notice is informational and is not legal advice.
