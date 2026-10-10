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
src/                      the playground (React + Vite + Tailwind v4)
  core/                   TypeScript mirror of the Rust crates
    lexer.ts  parser.ts  sema.ts  ast.ts  driver.ts
    backend/wasm.ts       WebAssembly binary encoder + code generator
    backend/c.ts          C99 emitter
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

### Two cores, one interface

The page asks `getCore()` for a compiler. If a Rust build has been loaded it
registers itself on `globalThis.__CAPL_CORE__` and is used; otherwise the
TypeScript mirror in `src/core/` takes over. Both implement the same pipeline
and the same result shape, so the UI cannot tell them apart — the badge in the
header shows which one is live.

```bash
./rust/build-wasm.sh          # wasm-pack build → public/capl/capl_wasm.js
cargo run -p capl-driver -- node.can --emit=wat
cargo run -p capl-driver -- node.can --emit=wasm -o node.wasm
cargo test --workspace
```

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
