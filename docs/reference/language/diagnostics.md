# Diagnostics and failures

> Normative for Loom language version 0.4 diagnostic and execution records.

Loom keeps source diagnostics, recoverable data errors, contract faults,
runtime faults, and implementation defects separate. A program can handle only
the failure channels represented as ordinary source values.

## Source diagnostics

Lexing, parsing, declaration analysis, type checking, obligation checking, and
validation report structured source diagnostics. An error prevents successful
checking or building. Human output has this leading form and may include a code
frame, related locations, and notes:

```text
main.loom:4:5: error[UnknownName]: unknown callable `missing`
```

Paths are project-relative. Line and column numbers are one-based. Byte offsets
are zero-based half-open UTF-8 byte positions; columns count Unicode scalar
values, not UTF-8 bytes or UTF-16 code units.

Diagnostics are ordered deterministically by path, source range, code, and
message. Related labels are likewise ordered by their own source positions and
labels.

## Machine-readable diagnostics

With JSON output enabled, each diagnostic is one JSON object on standard output
and standard error is empty for source diagnostics. The schema is:

```json
{
  "schema_version": 1,
  "category": "diagnostic",
  "severity": "error",
  "code": "UnknownName",
  "message": "unknown callable `missing`",
  "primary_span": {
    "path": "main.loom",
    "start_byte": 42,
    "end_byte": 51,
    "start_line": 4,
    "start_column": 5,
    "end_line": 4,
    "end_column": 14
  },
  "related": [],
  "notes": [],
  "details": {}
}
```

`severity` is `error`, `warning`, or `info`. `related` entries contain a
`label` and another span with the same shape. Consumers should select records by
`category` rather than assuming that every JSON line is a diagnostic.

## Common static diagnostic families

Diagnostic codes are stable identifiers intended for tooling. Representative
codes include:

| Area | Codes |
| --- | --- |
| source text | `InvalidUtf8`, `InvalidSourceCharacter`, `NewlineInString`, `UnterminatedString`, `InvalidEscape`, `InvalidUnicodeEscape`, `InvalidIntegerLiteral`, `InvalidFloatLiteral` |
| file grammar | `UnexpectedToken`, `SyntaxNestingLimit`, `ChainedComparison` |
| names and packages | `UnknownName`, `NameNotVisible`, `DuplicateDeclaration`, `ModuleCycle` |
| typing and inference | `TypeMismatch`, `CannotInferType`, `CannotInferListElement`, `InvalidGenericOperation`, `InvalidAssignmentTarget` |
| values and patterns | `UnusedValue`, `NonExhaustiveMatch`, `UnreachableMatchArm`, `TupleArityMismatch` |
| constrained data | `ConstraintUnsatisfied`, `InvariantUnsatisfied`, `InvalidContractExpression` |
| concepts | `MissingConformance`, `DuplicateConformance`, `OverlappingConformance`, `ConformanceResolutionCycle`, `ConformanceSignatureMismatch` |
| dynamic interfaces | `DynNotDeclared`, `DynStaticRequirement`, `DynGenericMethod`, `DynSelfLeak`, `DynAssociatedTypeUnbound`, `IllegalDynConversion` |
| resources | `MustScopeRequiresScoped`, `ScopedValueCopy`, `ManualDisposeOfScopedValue`, `NoSuspendAcrossAwait`, `CannotDiscardUnknownType` |
| tasks | `AwaitOutsideAsync`, `AwaitRequiresAsyncCall`, `UnawaitedAsyncCall`, `TaskAlreadyConsumed`, `TaskConditionallyConsumed`, `TaskJoinRequiresTasks` |
| Result propagation | `PropagationRequiresResult`, `PropagationRequiresResultReturn`, `PropagationErrorTypeMismatch`, `PropagationInCleanup` |

A diagnostic may carry related labels pointing to the first declaration,
required signature, conflicting conformance, or scope that established an
obligation. The human message may become more specific while the code remains
the machine key.

## Recoverable value errors

`Option` and `Result` are ordinary data. Their `None` and `Err` values do not
become execution-failure records. A test that returns `Err` fails, but the Err
remains the test's returned value rather than becoming a contract or runtime
fault.

`ConstraintError` is also ordinary Result data returned when an unproven
constrained type or record invariant fails during construction. Its structured
runtime value has this exact compiler-private shape:

```text
target_type   Text
code          Text
predicate     Text
path          List[Text]
value_summary Text
contract_span (Int, Int, Int)
```

`value_summary` is a stable type-only category or nominal type name. It never
contains scalar values, text or byte contents, lengths, collection counts,
enum variants, or nested business data. This non-disclosure rule is identical
in interpreted and native execution.

## Contract faults

A failed `requires`, `ensures`, invariant boundary, or `assert` terminates
ordinary control flow as a `ContractFault`. Its structured fields are:

```text
code          PreconditionFault | PostconditionFault | InvariantFault | AssertionFault
category      precondition | postcondition | invariant | assertion
message       human-readable description
contractSpan  source span of the violated contract
blameSpan     caller or implementation span according to the contract kind
```

Source code cannot catch a ContractFault or convert it implicitly to `Err`.
Test runners and task joins may report or capture it at their defined boundary.

## Runtime faults and defects

A `RuntimeFault` reports a defined runtime failure outside ordinary Result data,
such as checked integer overflow, integer division by zero, an out-of-range
`Bytes.add` unit, an invalid sleep duration, `TaskAnyFailed` when `Task.any` has
no successful child, or a faulting I/O operation. It contains `code`, `message`,
and `span`.

An execution defect reports a violation of the compiler/runtime contract rather
than a user program condition. It uses the `defect` channel and contains a defect
record with the same identifying fields. Programs cannot catch runtime faults
or defects.

## Run-failure JSON

Machine-readable execution failure is wrapped as:

```json
{
  "schema_version": 1,
  "category": "run_failure",
  "entry": "main",
  "failure": {
    "channel": "runtime",
    "fault": {
      "code": "IntegerDivisionByZero",
      "message": "integer division by zero",
      "span": {
        "file": 0,
        "range": { "start": 70, "end": 75 }
      }
    }
  }
}
```

`failure.channel` is `contract`, `runtime`, or `defect`. Contract records use a
`fault` member, runtime records use a `fault` member, and defects use a `defect`
member. Interpreter and native execution use the same failure shape.

A `TaskOutcome.Faulted(TaskFault)` is a source value produced by a structured
join. It exposes stable text through `.code()` and `.message()` but is not the
top-level `run_failure` envelope.
