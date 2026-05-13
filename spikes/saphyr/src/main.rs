//! saphyr-parser textual-edit spike (Option B).
//!
//! Throwaway probe for M2 Tasks 1.2–1.4 of `openspec/changes/add-safe-writes`.
//! See [`design.md`](../../../openspec/changes/add-safe-writes/design.md) D1,
//! D4, D11 for the rationale: we use `saphyr-parser`'s low-level event API to
//! collect a `Pointer → ByteRange` span map, then mutate the original byte
//! buffer by splicing the value range. **No emitter** is involved; surrounding
//! comments / blank lines / quote styles are preserved by construction
//! because we never rewrite them.
//!
//! NOT production code — `unwrap`/`anyhow::Context` everywhere is acceptable.
//! Output convention: `println!` for primary user-facing output (PASS/FAIL,
//! mutated bytes, span dump), `eprintln!` for diagnostics (warnings, diffs).
//!
//! Subcommands:
//!   span-build <FIXTURE>
//!   mutate <FIXTURE> <POINTER> <NEW_VALUE>
//!   assert-byte-perfect <FIXTURE> <POINTER> <NEW_VALUE>
//!   bench-span-build <FIXTURE>
//!   insert-test <FIXTURE>

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::ops::Range;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use camino::Utf8PathBuf;
use clap::{Args, Parser as ClapParser, Subcommand};
use saphyr_parser::{Event, Parser as YamlParser, ScalarStyle, Span};
use similar::{ChangeTag, TextDiff};

// =============================================================================
// CLI
// =============================================================================

#[derive(Debug, ClapParser)]
#[command(
    name = "saphyr-spike",
    about = "Textual-edit (span-based) YAML mutation viability spike"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse FIXTURE, build a Pointer→ValueSpan map, dump it on stdout.
    SpanBuild(SpanBuildArgs),
    /// Splice NEW_VALUE into the value range located by POINTER, write the
    /// resulting bytes to stdout. Bytes outside the value range are
    /// preserved verbatim.
    Mutate(MutateArgs),
    /// Mutate FIXTURE at POINTER with NEW_VALUE, then `similar`-diff the
    /// result vs the original. PASS iff exactly one line was inserted and
    /// one line removed (i.e. the mutation touched exactly one line and
    /// nothing else moved).
    AssertBytePerfect(MutateArgs),
    /// Build the span map for FIXTURE 10 times. Print median + stddev.
    BenchSpanBuild(SpanBuildArgs),
    /// Synthesise an insertion of `/spec/strategy/type RollingUpdate` into
    /// FIXTURE (only `a_k8s_with_comments.yaml` is supported), parse the
    /// result via `serde_norway::Value`, PASS iff parse succeeds.
    InsertTest(SpanBuildArgs),
}

#[derive(Debug, Args)]
struct SpanBuildArgs {
    fixture: Utf8PathBuf,
}

#[derive(Debug, Args)]
struct MutateArgs {
    fixture: Utf8PathBuf,
    /// JSON Pointer (RFC 6901). `/` separator. `~0` for `~`, `~1` for `/`.
    /// For multi-document streams, the first segment is the doc index
    /// (e.g. `/1/spec/ports/0/port`).
    pointer: String,
    /// Verbatim replacement text — substituted as-is into the value range.
    /// The spike does **not** quote-style-detect; the caller is expected
    /// to pass an already-rendered scalar (e.g. `"Updated"` with quotes).
    new_value: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::SpanBuild(args) => run_span_build(&args),
        Command::Mutate(args) => run_mutate(&args),
        Command::AssertBytePerfect(args) => run_assert_byte_perfect(&args),
        Command::BenchSpanBuild(args) => run_bench(&args),
        Command::InsertTest(args) => run_insert_test(&args),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(2)
        }
    }
}

// =============================================================================
// Core types
// =============================================================================

/// Byte-range location for a single YAML scalar, recorded against its
/// canonical RFC 6901 pointer.
#[derive(Debug, Clone)]
struct ValueSpan {
    /// Bytes covering the value literal in the source (the substring that
    /// `set_at` would replace). For `replicas: 3 # comment`, this is the
    /// `3` (1 byte). For `title: "Hello"`, this is `"Hello"` (7 bytes).
    value_range: Range<usize>,
    /// Bytes covering the entire logical line(s) holding key + value +
    /// trailing comment. Used by `del_at` (D4 in design.md). Best-effort
    /// in this spike — line scan is naive (newline-bounded).
    line_range: Range<usize>,
    /// 1-indexed column of the value start (used as proxy for indent in
    /// the production design).
    indent: u32,
    /// Block vs flow context — distinguishes how a replacement scalar
    /// should be quoted in production. The spike only uses this for
    /// diagnostics.
    context: SpanContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Flow* variants reserved for production renderer (D14).
enum SpanContext {
    /// Value position of a block-style mapping (`key: value`).
    BlockMapValue,
    /// Item of a block-style sequence (`- item`).
    BlockSeqItem,
    /// Value position of a flow-style mapping (`{key: value}`).
    FlowMapValue,
    /// Item of a flow-style sequence (`[item]`).
    FlowSeqItem,
}

/// Pointer → span. BTreeMap (vs IndexMap) is used in this spike because
/// it's std-only; production design (D4, open question 2) will revisit.
type SpanMap = BTreeMap<String, ValueSpan>;

// =============================================================================
// span-build
// =============================================================================

fn run_span_build(args: &SpanBuildArgs) -> Result<ExitCode> {
    let bytes = read_bytes(&args.fixture)?;
    let text = std::str::from_utf8(&bytes).context("fixture is not valid UTF-8")?;
    let spans =
        build_span_map(text).with_context(|| format!("span build failed for {}", args.fixture))?;

    for (pointer, span) in &spans {
        println!(
            "{pointer}\tvalue={start}..{end}\tline={lstart}..{lend}\tindent={ind}\tctx={ctx:?}",
            start = span.value_range.start,
            end = span.value_range.end,
            lstart = span.line_range.start,
            lend = span.line_range.end,
            ind = span.indent,
            ctx = span.context,
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Walks the YAML event stream and collects a `Pointer → ValueSpan` map.
///
/// State machine:
/// - Stack of `Frame`. Each frame describes "what we're collecting inside".
/// - For each `Scalar` event, decide: is this a key, or a value? In a
///   `Mapping` frame the first scalar after `MappingStart` (or after the
///   previous value finished) is a key — we stash its name. The next
///   "completion event" (Scalar / MappingStart / SequenceStart) is the
///   value, and gets recorded against `<parent_pointer>/<key>`.
/// - In a `Sequence` frame every "completion event" is an item; index
///   advances after each.
/// - Documents: pointer prefix `/<doc_index>` is added only if the
///   stream has > 1 document (matches the convention used in fixtures —
///   `a_k8s_with_comments.yaml` uses `/spec/replicas` while
///   `d_multi_doc.yaml` uses `/1/spec/ports/0/port`).
fn build_span_map(text: &str) -> Result<SpanMap> {
    // First pass: count documents so we know whether to emit the doc-index
    // prefix. We have to allocate twice but parsing 1 MB is cheap (<5 ms,
    // see `bench-span-build`).
    let n_docs = count_documents(text)?;
    let multi_doc = n_docs > 1;

    let mut spans = SpanMap::new();
    let mut state = State::new(text, multi_doc);

    let mut parser = YamlParser::new_from_str(text);
    while let Some(item) = parser.next_event() {
        let (event, span) = item.context("parse error")?;
        state.observe(event, span, &mut spans)?;
    }

    Ok(spans)
}

fn count_documents(text: &str) -> Result<usize> {
    let mut parser = YamlParser::new_from_str(text);
    let mut n = 0usize;
    while let Some(item) = parser.next_event() {
        let (event, _) = item.context("parse error")?;
        if matches!(event, Event::DocumentStart(_)) {
            n += 1;
        }
    }
    Ok(n)
}

/// Stack frame describing what we're building right now.
#[derive(Debug)]
enum Frame {
    /// A mapping. `pending_key` is `Some` once we've seen the key scalar
    /// and are waiting for the value event.
    Mapping { pending_key: Option<String> },
    /// A sequence. `index` is the position of the *next* item.
    Sequence { index: usize },
}

struct State<'src> {
    text: &'src str,
    multi_doc: bool,
    /// Index of the document currently being parsed (incremented on
    /// `DocumentStart`).
    doc_index: usize,
    /// Doc index has not yet been incremented at start; we use this flag
    /// so `DocumentStart` can set the index to 0 first time, then 1, ...
    seen_first_doc: bool,
    /// Path frames. The top of the stack is the innermost container.
    stack: Vec<Frame>,
    /// Path components (escaped). Mirrors `stack` for fast pointer build.
    path: Vec<String>,
}

impl<'src> State<'src> {
    fn new(text: &'src str, multi_doc: bool) -> Self {
        Self {
            text,
            multi_doc,
            doc_index: 0,
            seen_first_doc: false,
            stack: Vec::new(),
            path: Vec::new(),
        }
    }

    /// React to one parser event.
    fn observe(&mut self, event: Event<'_>, span: Span, spans: &mut SpanMap) -> Result<()> {
        match event {
            Event::StreamStart | Event::StreamEnd | Event::Nothing => {}
            Event::DocumentStart(_) => {
                if self.seen_first_doc {
                    self.doc_index += 1;
                }
                self.seen_first_doc = true;
                if self.multi_doc {
                    self.path.push(self.doc_index.to_string());
                }
            }
            Event::DocumentEnd => {
                if self.multi_doc {
                    self.path.pop();
                }
            }
            Event::Scalar(value, style, _anchor, _tag) => {
                // Is this a key or a value? We are a key iff our parent
                // is a Mapping frame with `pending_key == None`.
                let is_key = matches!(
                    self.stack.last(),
                    Some(Frame::Mapping { pending_key: None })
                );
                if is_key {
                    let key_str = pointer_escape(value.as_ref());
                    if let Some(Frame::Mapping { pending_key }) = self.stack.last_mut() {
                        *pending_key = Some(key_str);
                    }
                } else {
                    // Value position. Build the pointer for THIS value,
                    // record the span, and advance the parent frame.
                    let pointer = self.value_pointer()?;
                    let value_range = span_to_range(span);
                    let line_range = self.compute_line_range(&value_range);
                    let indent = u32::try_from(span.start.col()).unwrap_or(0);
                    let context = self.value_context(style);
                    spans.insert(
                        pointer,
                        ValueSpan {
                            value_range,
                            line_range,
                            indent,
                            context,
                        },
                    );
                    self.complete_value();
                }
            }
            Event::Alias(_) => {
                // Alias is a value — but saphyr-parser does NOT emit a
                // Span pointing back at the `*name` text in a useful way
                // (the span we get is for the alias use-site, which IS
                // what we want, but the value to splice would be the
                // entire alias including `*` and the name).
                //
                // For the spike we record nothing for aliases — none of
                // our test pointers target an alias. This is a key
                // finding logged in RESULTS.md / the spike report.
                eprintln!(
                    "WARN: skipping alias at line {} col {} \
                     (textual-edit of aliases is out of scope for this spike)",
                    span.start.line(),
                    span.start.col()
                );
                // Still advance the parent frame — the alias took up a
                // value slot.
                self.complete_value();
            }
            Event::SequenceStart(_anchor, _tag) => {
                // Record a "container value" span keyed by the parent
                // pointer? No — D4 says ValueSpan represents a SCALAR
                // value, not a container. For the spike we only record
                // spans we'd need to splice; insertion (D14) handles
                // missing intermediate containers separately.
                self.enter_container();
                self.stack.push(Frame::Sequence { index: 0 });
            }
            Event::SequenceEnd => {
                self.leave_container();
            }
            Event::MappingStart(_anchor, _tag) => {
                self.enter_container();
                self.stack.push(Frame::Mapping { pending_key: None });
            }
            Event::MappingEnd => {
                self.leave_container();
            }
        }
        Ok(())
    }

    /// Pointer for the value we're about to record. Pushes the relevant
    /// path component onto `self.path` (the caller is responsible for
    /// popping it via `complete_value`).
    fn value_pointer(&mut self) -> Result<String> {
        match self.stack.last_mut() {
            Some(Frame::Mapping { pending_key }) => {
                let key = pending_key
                    .take()
                    .ok_or_else(|| anyhow!("mapping value with no key"))?;
                self.path.push(key);
            }
            Some(Frame::Sequence { index }) => {
                let idx = *index;
                self.path.push(idx.to_string());
                *index = idx + 1;
            }
            None => {
                // Top-level scalar (e.g. a YAML doc that's a single scalar).
                // Pointer is `""` (root) or `/<doc>` for multi-doc.
            }
        }
        Ok(format!("/{}", self.path.join("/")))
    }

    /// Called immediately after we've recorded a value's span. Pops the
    /// path component pushed by `value_pointer`. Also re-arms the parent
    /// mapping frame (so the next scalar is a key again).
    fn complete_value(&mut self) {
        if !self.path.is_empty()
            && matches!(
                self.stack.last(),
                Some(Frame::Mapping { .. } | Frame::Sequence { .. })
            )
        {
            self.path.pop();
        }
    }

    /// Called on `MappingStart` / `SequenceStart`. The container itself
    /// occupies a value slot in the parent frame; we push the relevant
    /// path component but do NOT pop it on `complete_value` (that
    /// happens on `MappingEnd` / `SequenceEnd`).
    fn enter_container(&mut self) {
        match self.stack.last_mut() {
            Some(Frame::Mapping { pending_key }) => {
                if let Some(key) = pending_key.take() {
                    self.path.push(key);
                }
            }
            Some(Frame::Sequence { index }) => {
                let idx = *index;
                self.path.push(idx.to_string());
                *index = idx + 1;
            }
            None => {
                // Top-level container — no path component to push (the
                // doc-index prefix, if any, was pushed by DocumentStart).
            }
        }
    }

    fn leave_container(&mut self) {
        let _ = self.stack.pop();
        // Pop the path component that was pushed by `enter_container`,
        // but only if there's one to pop AND we're not at the doc-index
        // boundary (those are managed by Document{Start,End}).
        let target_len = self.doc_prefix_len();
        if self.path.len() > target_len {
            self.path.pop();
        }
    }

    /// Number of path components currently occupied by the doc-index
    /// prefix (0 for single-doc, 1 for multi-doc).
    fn doc_prefix_len(&self) -> usize {
        if self.multi_doc { 1 } else { 0 }
    }

    fn value_context(&self, style: ScalarStyle) -> SpanContext {
        // saphyr-parser's ScalarStyle does not expose block-vs-flow
        // directly — Plain/SingleQuoted/DoubleQuoted can appear in
        // either context. The flow distinction is implicit in the
        // surrounding events (we'd track it on Sequence/MappingStart's
        // style if the API exposed it; it doesn't). For the spike we
        // approximate from the parent frame and the scalar style.
        let in_seq = matches!(self.stack.last(), Some(Frame::Sequence { .. }));
        let _ = style; // currently unused — see comment above.
        // Conservatively, we report Block contexts. Flow contexts will
        // be added in production once we observe the surrounding flow
        // markers via byte scan or wait for an upstream API extension.
        if in_seq {
            SpanContext::BlockSeqItem
        } else {
            SpanContext::BlockMapValue
        }
    }

    /// Best-effort: span the entire logical line (\n-bounded) holding
    /// the value. Multi-line block scalars (`|`/`>`) would need extra
    /// work that the spike does not attempt.
    fn compute_line_range(&self, value_range: &Range<usize>) -> Range<usize> {
        let bytes = self.text.as_bytes();
        let mut start = value_range.start;
        while start > 0 && bytes[start - 1] != b'\n' {
            start -= 1;
        }
        let mut end = value_range.end;
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        if end < bytes.len() {
            end += 1; // include trailing newline
        }
        start..end
    }
}

fn pointer_escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn span_to_range(span: Span) -> Range<usize> {
    span.start.index()..span.end.index()
}

// =============================================================================
// mutate
// =============================================================================

fn run_mutate(args: &MutateArgs) -> Result<ExitCode> {
    let bytes = read_bytes(&args.fixture)?;
    let mutated = mutate_bytes(&bytes, &args.pointer, &args.new_value)
        .with_context(|| format!("mutate failed for {}", args.fixture))?;

    // Print raw bytes; do NOT use println! (it would add a trailing newline).
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&mutated)
        .context("failed to write mutated bytes to stdout")?;
    Ok(ExitCode::SUCCESS)
}

fn mutate_bytes(original: &[u8], pointer: &str, new_value: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(original).context("fixture is not valid UTF-8")?;
    let spans = build_span_map(text)?;
    let span = spans
        .get(pointer)
        .ok_or_else(|| anyhow!("pointer not found: {pointer}"))?
        .clone();
    let mut out = Vec::with_capacity(original.len().saturating_add(new_value.len()));
    out.extend_from_slice(&original[..span.value_range.start]);
    out.extend_from_slice(new_value.as_bytes());
    out.extend_from_slice(&original[span.value_range.end..]);
    Ok(out)
}

// =============================================================================
// assert-byte-perfect
// =============================================================================

fn run_assert_byte_perfect(args: &MutateArgs) -> Result<ExitCode> {
    let bytes = read_bytes(&args.fixture)?;
    let mutated = mutate_bytes(&bytes, &args.pointer, &args.new_value)
        .with_context(|| format!("mutate failed for {}", args.fixture))?;

    let original_str = std::str::from_utf8(&bytes).context("fixture is not valid UTF-8")?;
    let mutated_str = std::str::from_utf8(&mutated).context("mutation produced invalid UTF-8")?;

    let diff = TextDiff::from_lines(original_str, mutated_str);
    let mut added = 0usize;
    let mut removed = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }

    if removed == 1 && added == 1 {
        println!("PASS {} {}={}", args.fixture, args.pointer, args.new_value);
        Ok(ExitCode::SUCCESS)
    } else {
        println!(
            "FAIL {} {}={}: removed={removed}, added={added}",
            args.fixture, args.pointer, args.new_value
        );
        eprintln!("--- diff for {} ---", args.fixture);
        let diff = TextDiff::from_lines(original_str, mutated_str);
        for change in diff.iter_all_changes() {
            let prefix = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            eprint!("{prefix}{change}");
        }
        eprintln!("--- end diff for {} ---", args.fixture);
        Ok(ExitCode::from(1))
    }
}

// =============================================================================
// bench-span-build
// =============================================================================

const BENCH_ITERATIONS: usize = 10;

fn run_bench(args: &SpanBuildArgs) -> Result<ExitCode> {
    let bytes = read_bytes(&args.fixture)?;
    let text = std::str::from_utf8(&bytes).context("fixture is not valid UTF-8")?;

    let mut samples_ms = Vec::with_capacity(BENCH_ITERATIONS);
    for _ in 0..BENCH_ITERATIONS {
        let start = Instant::now();
        let spans = build_span_map(text)?;
        let elapsed = start.elapsed();
        std::hint::black_box(&spans);
        samples_ms.push(elapsed.as_secs_f64() * 1000.0);
    }

    let median = median_ms(&mut samples_ms);
    let stddev = stddev_ms(&samples_ms);
    println!("median: {median:.3}ms, stddev: {stddev:.3}ms, iterations: {BENCH_ITERATIONS}");
    Ok(ExitCode::SUCCESS)
}

fn median_ms(samples: &mut [f64]) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = samples.len();
    if n == 0 {
        return f64::NAN;
    }
    if n.is_multiple_of(2) {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    } else {
        samples[n / 2]
    }
}

fn stddev_ms(samples: &[f64]) -> f64 {
    let n = samples.len();
    if n < 2 {
        return 0.0;
    }
    let mean: f64 = samples.iter().sum::<f64>() / n as f64;
    let variance: f64 = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    variance.sqrt()
}

// =============================================================================
// insert-test (Task 1.4)
// =============================================================================

/// Hardcoded insertion of `/spec/strategy/type RollingUpdate` into the
/// k8s fixture, with mkdir-p-style parent creation. The implementation is
/// intentionally simple — it is a probe, not a renderer:
/// 1. Span-build the fixture; assert the target pointer is absent.
/// 2. Locate the nearest existing ancestor (`/spec`).
/// 3. Render a synthetic suffix (`\n  strategy:\n    type: RollingUpdate`)
///    keyed off the parent's `indent + 2`.
/// 4. Find the byte position at which the parent's last child line ends —
///    naive scan of the remaining lines until an indent ≤ parent's is
///    encountered (or EOF).
/// 5. Splice and validate via `serde_norway::from_slice::<serde_norway::Value>`.
fn run_insert_test(args: &SpanBuildArgs) -> Result<ExitCode> {
    let bytes = read_bytes(&args.fixture)?;
    let text = std::str::from_utf8(&bytes).context("fixture is not valid UTF-8")?;
    let spans = build_span_map(text)?;

    const TARGET: &str = "/spec/strategy/type";
    const PARENT: &str = "/spec";

    if spans.contains_key(TARGET) {
        println!("FAIL insert-test: target pointer {TARGET} already exists");
        return Ok(ExitCode::from(1));
    }
    let parent_span = spans
        .get(PARENT)
        .or_else(|| spans.get("/spec/replicas"))
        .ok_or_else(|| {
            anyhow!(
                "insert-test only supports fixtures with a /spec/* mapping; \
                 fixture {} doesn't qualify",
                args.fixture
            )
        })?
        .clone();

    // The parent is itself a mapping — but our span map records spans for
    // SCALAR values only (D4). So we infer the parent block by scanning
    // forward from the end of the deepest known child of /spec until we
    // hit a line whose indent is ≤ /spec's child indent (i.e. a sibling
    // of /spec).
    //
    // We need the *key* column of /spec's children, not the *value*
    // column we recorded in `parent_span.indent`. Scan the line holding
    // /spec/replicas backward from `value_range.start` to count leading
    // whitespace — that's the key indent.
    let child_key_col = key_indent_col(text.as_bytes(), &parent_span);
    let parent_block_end =
        find_block_end(text.as_bytes(), parent_span.line_range.end, child_key_col);
    let new_key_indent_spaces = (child_key_col.saturating_sub(1)) as usize;
    let nested_indent_spaces = new_key_indent_spaces + 2;
    let suffix = format!(
        "{indent_outer}strategy:\n{indent_inner}type: RollingUpdate\n",
        indent_outer = " ".repeat(new_key_indent_spaces),
        indent_inner = " ".repeat(nested_indent_spaces),
    );

    let mut out = Vec::with_capacity(bytes.len().saturating_add(suffix.len()));
    out.extend_from_slice(&bytes[..parent_block_end]);
    out.extend_from_slice(suffix.as_bytes());
    out.extend_from_slice(&bytes[parent_block_end..]);

    // Validate: the result must parse as YAML.
    match serde_norway::from_slice::<serde_norway::Value>(&out) {
        Ok(_) => {
            println!("PASS insert-test");
            // Echo the result on stdout for human inspection.
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(&out)
                .context("failed to write inserted output")?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            println!("FAIL insert-test: {err}");
            eprintln!("--- inserted output ---");
            eprintln!("{}", String::from_utf8_lossy(&out));
            eprintln!("--- end inserted output ---");
            Ok(ExitCode::from(1))
        }
    }
}

/// Walk backward from `parent_span.value_range.start` to the start of
/// the line, then forward through that line counting leading whitespace
/// — the result is the 1-indexed column at which the *key* begins.
fn key_indent_col(bytes: &[u8], parent_span: &ValueSpan) -> u32 {
    let line_start = parent_span.line_range.start;
    let mut col = 1u32;
    let mut i = line_start;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        col += 1;
        i += 1;
    }
    col
}

/// Forward scan from `from` through `bytes`, returning the byte position
/// at which a "sibling-or-shallower" line begins (or EOF). Lines whose
/// first non-whitespace column is < `child_col` (1-indexed) terminate
/// the parent block. Blank / comment lines are skipped.
fn find_block_end(bytes: &[u8], from: usize, child_col: u32) -> usize {
    let child_col = child_col as usize;
    let mut pos = from;
    let mut last_content_end = from;
    while pos < bytes.len() {
        let line_start = pos;
        // Find end of line.
        let mut line_end = line_start;
        while line_end < bytes.len() && bytes[line_end] != b'\n' {
            line_end += 1;
        }
        let line_with_nl_end = if line_end < bytes.len() {
            line_end + 1
        } else {
            line_end
        };

        // Compute first non-whitespace column (1-indexed).
        let line_slice = &bytes[line_start..line_end];
        let first_non_ws = line_slice.iter().position(|b| *b != b' ' && *b != b'\t');
        match first_non_ws {
            None => {
                // Blank line — keep scanning, don't update last_content_end.
                pos = line_with_nl_end;
                continue;
            }
            Some(off) => {
                let col = off + 1;
                if line_slice[off] == b'#' {
                    // Comment line — skip without touching last_content_end.
                    pos = line_with_nl_end;
                    continue;
                }
                if col < child_col {
                    // Sibling of (or shallower than) the parent → stop.
                    return last_content_end;
                }
                last_content_end = line_with_nl_end;
                pos = line_with_nl_end;
            }
        }
    }
    last_content_end
}

// =============================================================================
// IO helpers
// =============================================================================

fn read_bytes(path: &Utf8PathBuf) -> Result<Vec<u8>> {
    fs::read(path.as_std_path()).with_context(|| format!("failed to read fixture {path}"))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: the span map for `/title: "Hello, dq"` should record a span
    /// covering the literal `"Hello, dq"` (with surrounding double quotes).
    #[test]
    fn span_covers_quoted_scalar_with_quotes() {
        let text = "title: \"Hello, dq\"\n";
        let spans = build_span_map(text).expect("span build");
        let span = spans.get("/title").expect("/title span");
        let value = &text.as_bytes()[span.value_range.clone()];
        assert_eq!(value, b"\"Hello, dq\"");
    }

    /// Sanity: bare scalar inside a nested block mapping resolves to the
    /// expected pointer and byte range.
    #[test]
    fn pointer_for_nested_mapping_value() {
        let text = "spec:\n  replicas: 3\n";
        let spans = build_span_map(text).expect("span build");
        let span = spans.get("/spec/replicas").expect("/spec/replicas span");
        let value = &text.as_bytes()[span.value_range.clone()];
        assert_eq!(value, b"3");
    }

    /// Sanity: sequence index pointer.
    #[test]
    fn pointer_for_sequence_item() {
        let text = "tags:\n  - rust\n  - cli\n";
        let spans = build_span_map(text).expect("span build");
        let span = spans.get("/tags/1").expect("/tags/1 span");
        let value = &text.as_bytes()[span.value_range.clone()];
        assert_eq!(value, b"cli");
    }

    /// Sanity: multi-doc stream uses doc-index prefix.
    #[test]
    fn pointer_for_multi_doc_uses_doc_index() {
        let text = "---\nname: a\n---\nname: b\n";
        let spans = build_span_map(text).expect("span build");
        assert!(spans.contains_key("/0/name"), "spans={spans:?}");
        assert!(spans.contains_key("/1/name"), "spans={spans:?}");
    }

    /// Sanity: pointer-not-found returns Err.
    #[test]
    fn mutate_unknown_pointer_errors() {
        let bytes = b"a: 1\n";
        let err = mutate_bytes(bytes, "/missing", "x").unwrap_err();
        assert!(err.to_string().contains("pointer not found"));
    }

    /// Sanity: a scalar mutation keeps everything else byte-identical.
    #[test]
    fn mutate_replaces_only_value_bytes() {
        let bytes = b"a: 1 # comment\nb: 2\n";
        let out = mutate_bytes(bytes, "/a", "5").expect("mutate");
        assert_eq!(out, b"a: 5 # comment\nb: 2\n");
    }
}
