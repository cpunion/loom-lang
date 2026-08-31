# Terminology

Use these terms consistently in code, diagnostics, and documentation.

| Term | Meaning |
| --- | --- |
| Loom | The language and project. Use `loom-lang` for the repository only when disambiguation is needed. |
| `loom` | The user-facing project and toolchain command. |
| Toolchain version | The Cargo/package version of compiler tools, currently `0.1.0`. |
| Language version | The source semantic version selected by `language` in `loom.toml`, currently `0.4`. |
| Manifest schema | The syntax/data version of `loom.toml`, independent of language version. |
| Lockfile schema | The data version of `loom.lock`. |
| Module | A named, versioned dependency unit rooted by `loom.toml`. The manifest directory is its source root. |
| Package | One source directory inside a module. Its import path is the module name plus relative directory segments; file names do not contribute. |
| Standalone input | A source file or directory compiled without `loom.toml`; it uses the synthetic `<standalone>@0` module identity and has no features, targets, or lockfile. |
| Target | A named manifest output selection: `bin` or `lib`. Do not confuse it with an LLVM target triple. Tests are source declarations, not targets. |
| Test companion | A compiler-owned package identity used only by `loom test`. It contains selected tests and test-only helpers and has one-way access to its production package's private members. |
| Entry | The selected public zero-argument, `Unit`-returning function for an executable. |
| Frontend | Discovery, syntax, HIR, semantic analysis, lowering, and MIR validation. |
| HIR | Source-independent high-level identities and bodies used by semantic analysis. |
| MIR | Typed executable intermediate representation. |
| Checked MIR | MIR that passed the independent validator; the backend trust boundary. |
| Backend | An executor/code generator consuming checked MIR: interpreter or LLVM. |
| Artifact | A concrete output. Qualify it as executable, object, `.loomi`, `.loomlib`, or runtime bundle. |
| `.loomi` | A versioned interpreted executable containing the closed checked-MIR definitions for one validated entry. |
| `.loomlib` | A versioned source-and-interface module whose embedded Loom source is recompiled by the consumer. |
| Root | A function selected by an executable/test build from which native reachability starts. |
| Reachability | Closed-world traversal of functions, witnesses, builtins, and used method slots. |
| DCE | Dead-code elimination. Use it for code/data removed from a final executable artifact, not for skipping frontend checks. |
| Concept | Loom's named behavioral abstraction. Do not introduce “trait” as an alternate language keyword. |
| Conformance | An explicit `impl C for T` proof that a type satisfies a concept. |
| Witness | Compiler evidence for one conformance and its method/associated-type bindings. |
| `dyn C` | A first-class value carrying a value and selected dynamic conformance behavior. “Interface value” may explain it, but is not syntax. |
| Universal `Value` | Compiler-private fallback native envelope. It is not a source type named `any`. |
| Native layout | A compiler-private machine representation chosen for an eligible static shape. |
| Runtime | `LoomRuntime`: managed heap, synchronous roots, and collector state. |
| Executor | Single-thread Task scheduler attached to one runtime when async execution needs it. |
| Reactor | Lazy OS readiness and timer registration component inside the executor. |
| Task | One-shot structured asynchronous computation, not a pull generator. |
| Suspension | A point where a Task stores live state and yields control to the executor. |
| `scoped` | A resource binding whose cleanup occurs at the end of the enclosing lexical block. |
| `defer` | An explicit cleanup block run at the end of its enclosing lexical block. |
| Registry cache | Validated cache of downloaded module bundles. Separate from compiler cache. |
| Compiler cache | Project-local content-addressed reuse for compiler layers and selected artifacts. |
| Runtime bundle | Versioned target runtime archive plus manifest used for explicit linking. |

Avoid “supported” without a qualifier. Prefer “CI-tested compiler layers,”
“CI-tested native runtime,” “LLVM object target,” or “published release
archive.”

Avoid calling compiler-private layouts “the Loom ABI.” Loom currently has no
stable public native ABI.
