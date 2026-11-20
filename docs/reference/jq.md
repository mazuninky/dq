# The embedded jq engine (`dq-transform`)

dq does not invent a query language. Where pointers are not enough —
conditions, transforms, arithmetic, aggregation — it embeds jq through
[jaq](https://github.com/01mf02/jaq), a jq implementation in Rust. The engine
lives in `crates/dq-transform/src/jq.rs` and is used by `dq query`,
`dq set --jq`, rule `match.filter`, `check.jq`, composite `extract`,
`fix.jq`, `fix.ops` and `loc.*`.

## Dialect

`jaq-core` 3 + `jaq-std` 3 + `jaq-json` 2 (with the `sync` feature, so values
are `Send + Sync`). The standard definitions are loaded: `map`, `select`,
`length`, `sort_by`, `group_by`, `keys`, `values`, `del`, `to_entries` /
`from_entries`, regex (`test`, `sub`, …), formats, math, time, `tojson` /
`fromjson`. Differences from jq 1.7 are jaq's, except path assignment, which
dq makes behave as in jq (below); dq adds no functions of its own beyond
the internal `_dq_*` helpers this relies on.
Command-line variables (`--arg`, `--argjson`, `--slurpfile`) are not
available.

## Path assignment

jq creates what a path assignment walks through and does not find; jaq 3
fails there with "cannot use null as iterable". dq follows jq, for `=`, `|=`,
`+=` `-=` `*=` `/=` `%=`, `//=`, `setpath`, `delpaths` and `del`:

| Program | Input | Output |
|---|---|---|
| `.a.b = 1` (and `\|=`, `+=`, `//=`) | `{}`, `null`, `{"a": null}` | `{"a":{"b":1}}` |
| `.a[2] = 1` | `{}` | `{"a":[null,null,1]}` |
| `.[3] \|= .` | `[1]` | `[1,null,null,null]` |
| `setpath(["a",0,"b"]; 1)` | `{}` | `{"a":[{"b":1}]}` |
| `del(.a.b)`, `del(.a[0])` | `{}` | `{}` |
| `del(.[0], .[1])` | `[1,2,3]` | `[3]` (paths deleted together) |
| `del(.[5])`, `del(.[-5])` | `[1,2,3]` | unchanged |

A null becomes `{}` before a string key and `[]` before an index; an array is
padded with nulls up to an index past its end, in place (building an array
item by item stays linear). As in jq, an index past 536870911 is the runtime
error "Array index too large" (exit 1) rather than an allocation of that size. What jq refuses stays refused
(exit 1): a key into a string or a number (`.a.b = 1` on `{"a":"x"}`), a
negative index past the start, iterating null (`.a[] = 1` on `{}`), and
`null - 1` for `.a.b -= 1` on `{}`. The right-hand side of `=` and `op=` sees
the input as it was (`.a.b = .a` on `{}` gives `{"a":{"b":null}}`).

How: `JqEngine::compile` parses the program and, when it holds an assignment
operator, prints it back with each `L op R` turned into
`(R) as $__dq_rhs | _dq_vivify(L) | L op $__dq_rhs` (`L |= F` into
`_dq_vivify(L) | L |= F`); `_dq_vivify` creates the containers every path of
`L` needs, then jaq's own update runs. On `L`, every index and slice bound
but a string literal goes through `_dq_index`, which turns a float with an
integral value into the integer (jaq's update refuses a float index).
`setpath`, `delpaths` and `del` are redefined as jq defines them, on the
natives `_dq_ensure` and `_dq_delpaths`.
Compile errors still point into the program as written. The code is in
`crates/dq-transform/src/jq/paths.rs`.

An index that is a float with an integral value is the integer, as in jq:
`.[2.0] = 1` and `.[1e3] = 1` on `null` give `[null,null,1]` and an array of
1001 items, `setpath([2.0]; 1)` and `delpaths([[1.0]])` take it too.

Left as jaq has it: a slice of null on the path (`.a[1:2] = [1]` on `{}`,
jq gives `{"a":[1]}`) and a fractional index fail (jq truncates: `.[1.5] = 1`
gives `[null,1]`); an integral float index outside an assignment's left-hand
side still fails as in jaq (`.[2.0]` read from an array, `del(.[1.0])`); `.a.b |= empty` on `{}`
gives `{"a":{}}` where jq gives `{}`.

## API

```rust
let engine = JqEngine::compile(".spec.replicas |= . + 1")?; // once
let out: Vec<serde_json::Value> = engine.run(&input)?;      // per input
let out: Vec<dq_core::Value>   = engine.run_value(&value)?; // per input, exact numbers
```

- `compile` parses and compiles once and never panics on bad input.
  `JqEngine` is `Send + Sync + Clone`, so one compiled engine is shared by
  every worker of a `--parallel` run.
- `run` / `run_value` evaluate against one input and collect the whole output
  stream in order.
- `run_value` works on `dq_core::Value` directly, without passing through
  `serde_json::Value`. This matters for numbers: NaN and infinities are jq
  numbers inside the filter (`.x | type` is `"number"`, `isnan` works) and
  come back as floats; a big integer comes back as `Int` or `BigInt`; a float
  literal stays a `Float` when an `f64` holds its decimal value and a
  `BigFloat` (its literal) otherwise, until the filter computes with it. A
  value the filter passes through untouched comes back as the same data.
- `serde_to_val` / `val_to_serde` convert `serde_json::Value` ↔ jaq values
  (the `arbitrary_precision` feature keeps big literals). `serde_json` cannot
  hold NaN or infinity, which is why dq's own callers use `run_value`; see
  below.
- `ir_to_val` / `val_to_owned_ir` are the IR-aware variants. jaq values carry
  no provenance, so `ir_to_val` drops it and `val_to_owned_ir` marks every
  output node `Synthetic { Computed }`. Callers that need positions emit
  pointers from their jq (for example `loc.pointer`) and look them up in the
  **input** IR. See [ir-and-edit-ops.md](ir-and-edit-ops.md).

## Errors

`JqError` with a stable `kind_name()`:

| Variant | `kind_name` | Meaning | CLI exit |
|---|---|---|---|
| `Compile { snippet, position, message }` | `compile` | the expression does not lex/parse/compile; `position` is the 0-based byte offset, `snippet` an excerpt of at most about 60 characters | 3 (converted to a parse error) |
| `Runtime { message }` | `runtime` | evaluation failed (type error, `null / 0`, …); no position | 1 |
| `Conversion { message }` | `conversion` | a result cannot become a dq value (invalid UTF-8 byte string, a key that formats to an empty string) | 1 |
| `FeatureDisabled { hint }` | `feature_disabled` | built without the engine | 1 |

## The `embedded-jq` feature

`dq-transform` gates the jaq crates behind the default feature
`embedded-jq`. Without it the crate still builds and every method returns
`FeatureDisabled { hint: "rebuild with --features embedded-jq" }`. The
original plan's fallback of shelling out to an external `jq` was not built.

## Non-finite and big numbers

Every jq call dq makes runs on the document's own value: `query`,
`set --jq`, `fix.jq` / `fix.ops`, rule `match.filter` and `check.jq`, `loc.*`,
composite `extract`, and the plugin host's `jq.eval`. So a NaN or an infinity
is a jq number everywhere (`type == "number"`, `isnan`, `isinfinite` hold) and
a big number keeps its exact value. How such numbers are *printed* depends on
the output format (see [read-commands.md](read-commands.md)); plugins reading
the document as JSON get them as strings ([../plugin-abi.md](../plugin-abi.md)).

## Performance

Rule expressions are compiled once per evaluator. Caching compiled
expressions across evaluators and commands is the subject of
[../plans/perf-jq-compile-cache.md](../plans/perf-jq-compile-cache.md).
