# CAPL Language Reference (Public-Source Notes)

This document is a development reference for CAPL Playground. It is not an
official Vector language specification and does not replace product
documentation. It combines publicly accessible material with the behavior
implemented in this repository. When the public material and compiler behavior
differ, the Rust tests and diagnostics are authoritative for this project.

## Sources

- [CAPL Scripting Quickstart (Vector Informatik)](https://cdn.vector.com/cms/content/products/VectorCAST/Events/TechNights/CAPLQuickstart_Generic_2018_Final.pdf)
- [CAPL Introduction (Vector Help)](https://help.vector.com/vTESTstudio/11/en/Help/Content/Topics/ProgrammingInterfaces/CAPL/CAPLIntroduction.htm)
- [CAPL Functions in Vector Tools (Vector Support)](https://support.vector.com/kb?id=kb_article_view&sysparm_article=KB0011332)

These sources describe CAPL in the Vector tool ecosystem. The complete set of
functions and object semantics varies with the host product, product version,
and database configuration. This project uses public material as a
compatibility reference rather than treating it as a stable, complete language
standard.

## Program Structure

A CAPL program commonly contains global variables, user functions, and event
handlers:

```capl
variables {
  long counter = 0;
  msTimer tick;
}

long twice(long value) {
  return value * 2;
}

on start {
  counter = twice(21);
  setTimer(tick, 100);
}
```

The Rust compiler in this repository handles `variables { ... }`, user
functions, `on ... { ... }` handlers, and ordinary statements.

## Types and Declarations

The parser accepts these scalar types:

```capl
char letter = 'A'; byte flags = 0; word rpm = 800; int small = -1;
long count = 0; int64 total = 0; qword mask = 0;
float gain = 1.0; double factor = 2.0; timer coarseTimer; msTimer preciseTimer;
long samples[4] = { 1, 2, 3, 4 };
char greeting[32] = "hello";
```

Scalar types, arrays, string initialization, message objects, and local
variables are covered. `enum` declarations and enum constants are connected
through the Rust parser, semantic pass, C backend, and WebAssembly backend;
enum values are represented as integer constants.

Named structures and named fields are also supported:

```capl
struct Sample {
  long count;
  byte flags[4];
};

variables { struct Sample state; }
on start {
  state.count = 1;
  state.flags[0] = 0xAA;
}
```

Structure layout is computed by the semantic pass. Arrays and structures at
function boundaries use storage addresses in the WASM backend. The C backend
uses array pointers and structure pointers for those boundaries; structure
return values are returned through a pointer. This is the current playground
ABI and is covered by the `aggregates.can` driver fixture.

Array return types use the `long[] function(...)` form in the supported subset
and follow the same address-based ABI:

```capl
long[] identity(long values[]) { return values; }
```

The native driver can expand relative includes before parsing:

```capl
includes { "shared.can" }
```

Include search paths are taken from `CAPL_INCLUDE_PATH`. Browser compilation
does not resolve files because the browser compiler has no filesystem access.

## Event Handlers

```capl
on preStart { write("loading"); }
on start { write("running"); }
on timer tick { write("timer"); }
on key 'r' { write("reset"); }
on message 0x100 { write("received"); }
on errorFrame { write("bus error"); }
```

This project supports `preStart`, `start`, `stopMeasurement`, `preStop`,
`timer`, `message`, `key`, `sysvar`, `envVar`, `errorFrame`, and `busOff`
events.

## Message Objects

```capl
variables { message 0x100 frame; }
on start {
  frame.dlc = 8;
  frame.byte(0) = 0x12;
  frame.word(1) = 0x3456;
  output(frame);
}
on message 0x100 {
  write("id=%X dlc=%d first=%X", this.id, this.dlc, this.byte(0));
}
```

The main lowering paths cover `.id`, `.dlc`, `.can`, `.dir`, `.rtr`, `.time`,
and `byte(n)`, `word(n)`, `dword(n)`, and `qword(n)`. The simulator host
dispatches frames emitted by `output()` back to matching message handlers.

## Control Flow and Built-ins

The compiler covers `if/else`, `while`, `do/while`, `for`, `switch`,
fall-through, `break`, `continue`, `return`, indexing, assignment, and compound
assignment. Recursive functions, short-circuit logic, and conditional
expressions are also covered by the current lowering paths.

Built-ins currently include `write`, `output`, `timeNow`, `setTimer`,
`setTimerCyclic`, `cancelTimer`, `random`, `abs`, `min`, `max`, `sqrt`, `pow`,
`sin`, `cos`, `floor`, `ceil`, `elCount`, `strlen`, `canOnline`, `canOffline`,
and `stopMeasurement`.

## Implementation and Testing

Every implemented feature should have at least one `.can` fixture and one
driver-level integration assertion. Existing fixtures live in
`rust/crates/capl-driver/tests/programs/`.

When adding syntax:

1. Document the smallest valid syntax and its implementation status here.
2. Add lexer, parser, and semantic tests for the relevant stages.
3. Add a real `.can` fixture and compile it through the C, WebAssembly, and AST
   backends.
4. Add a browser runtime test for events and host functions where applicable.
5. Mark the feature as implemented only after all relevant stages pass.

## Explicit Non-Goals

This project does not currently promise support for anonymous or packed
structures, database signals, `$Signal`, test modules, the complete product
API, every system-variable type, or Vector-tool-specific extensions. Aggregate
boundaries use the documented address ABI rather than claiming compatibility
with every CAPL by-value rule.
