//! HCL 2 (HashiCorp Configuration Language) parser and writer.
//!
//! M5 v1: read+best-effort write. Comments, operator spacing, and original
//! number-literal formatting are NOT preserved on the write path — see
//! design D1 / D11. Block syntax `block_type "label" { ... }` becomes
//! `Map { block_type: { label: { ... } } }` (one nesting level per label),
//! per the format-support spec scenario "HCL backend block parses to
//! nested map". Blocks repeated with the same type and labels become a
//! list of their bodies, in source order, under that same key:
//! `ingress { p = 1 }` twice is `{ ingress: [{ p: 1 }, { p: 2 }] }`. The
//! document records which keys were blocks, and the writer writes those
//! back as blocks; it records the source of every expression too
//! ([`HclLayout`]), and the writer writes an expression whose value is
//! still what it read as back as that source.
//!
//! This parser produces a document with no spans, so no textual edits:
//! `Document::set_at` / `del_at` against an HCL document change its value
//! tree and write it whole anew through [`Format::write`], blocks kept
//! blocks, comments and layout dropped (see
//! `Document::edits_by_rewrite`).

use std::collections::HashMap;
use std::io::Write;

use camino::Utf8PathBuf;
use hcl::edit::Span as _;
use hcl::edit::structure::{
    Block as EditBlock, BlockLabel as EditLabel, Body as EditBody, Structure as EditStructure,
};
use hcl::{Expression, Number, Value as HclValue};
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, Layout, Value};
use crate::error::Error;
use crate::format::Format;

/// HCL format implementation.
#[derive(Debug, Clone, Copy)]
pub struct Hcl;

impl Format for Hcl {
    fn name(&self) -> &'static str {
        "hcl"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["hcl", "tf", "tfvars"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in HCL input: {e}"),
        })?;
        // One parse, with `hcl-edit`, which keeps every number literal;
        // `hcl::parse` is the same parse turned into `hcl-rs` types.
        let body: EditBody = text
            .parse()
            .map_err(|e: hcl::edit::parser::Error| map_hcl_parse_error(e.into()))?;
        let mut reader = Reader {
            text,
            path: Vec::new(),
            layout: HclLayout::default(),
        };
        let (value, notes) = reader.body_to_value(&body)?;
        for (at, note) in notes {
            match note {
                Note::Expr(expr) => {
                    reader.layout.exprs.insert(at, expr);
                }
                Note::Key(source) => {
                    reader.layout.keys.insert(at, source);
                }
            }
        }
        Ok(Document::value_only(value, FormatTag::Hcl)
            .with_layout(Layout::Hcl(reader.layout))
            .with_source_bytes(bytes))
    }

    fn write(&self, doc: &Document, w: &mut dyn Write) -> Result<()> {
        // Comments are not preserved (M5 v1). The `hcl-rs` `Number` type
        // cannot carry an arbitrary-precision literal: a big number past
        // `u64` / `f64` is refused rather than written as another value.
        let serialized = match doc.value() {
            Value::Map(map) => write_body(
                map,
                match doc.layout() {
                    Layout::Hcl(layout) => Some(layout),
                    _ => None,
                },
            )?,
            other => {
                return Err(Error::Format {
                    format: "hcl",
                    message: hcl_root_refusal(other),
                });
            }
        };
        w.write_all(serialized.as_bytes())
            .map_err(|source| Error::Io {
                path: Utf8PathBuf::from("<hcl-writer>"),
                source,
            })
    }
}

/// What the HCL reader saw of a file beyond its value tree, for the
/// writer to write back ([`Layout::Hcl`]): which keys are blocks, and the
/// source of every expression.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct HclLayout {
    /// Which keys were read as blocks, with how many labels.
    ///
    /// A key is addressed by its path from the root: the keys down to the
    /// body it is in — block types, labels and attribute names, but not
    /// the index of a repeated block, so every repeat shares one path —
    /// then the key itself. Bodies that share a path because their blocks
    /// repeat may disagree on a key's label count (`x { y "a" {} }`,
    /// `x { y {} }`); such a key is written as an attribute, which reads
    /// back as the same tree.
    blocks: HashMap<Vec<String>, Option<usize>>,
    /// Every value read from an expression a value cannot spell — a
    /// traversal, a function call, an operator, a conditional, a `for`, a
    /// template with an interpolation or a directive, a heredoc, a
    /// parenthesis — or from a string holding `${` or `%{`, by its exact
    /// path from the root (the index of a repeated block or of an array
    /// item included): what it read as, and its source text.
    exprs: HashMap<Vec<String>, Expr>,
    /// The source of every object key read from an expression
    /// (`(var.k) = 1`), by the exact path of its member — the key is the
    /// string it read as, the last segment. Written as a string, such a key
    /// would be a literal key, no longer the expression.
    keys: HashMap<Vec<String>, String>,
    /// Whether the tree written may have moved values since they were
    /// read (`set --jq`, `fix.jq`): an expression's value is then also
    /// looked for away from its place. A point edit moves nothing it does
    /// not account for ([`HclLayout::after_edit`]).
    moved: bool,
}

/// One expression the reader saw: what it read as, and its source.
#[derive(Debug, Clone, PartialEq)]
struct Expr {
    value: Value,
    source: String,
}

impl HclLayout {
    /// Note that the key `path` ends in is a block type with `labels`
    /// labels.
    fn record(&mut self, path: &[String], labels: usize) {
        self.blocks
            .entry(path.to_vec())
            .and_modify(|seen| {
                if *seen != Some(labels) {
                    *seen = None;
                }
            })
            .or_insert(Some(labels));
    }

    /// How many labels the blocks of the key `path` ends in carry, if that
    /// key was read as a block type with one label count.
    fn labels_at(&self, path: &[String]) -> Option<usize> {
        self.blocks.get(path).copied().flatten()
    }

    /// This layout, for a tree a transform may have moved values in.
    pub(crate) fn moved(&self) -> Self {
        Self {
            moved: true,
            ..self.clone()
        }
    }

    /// This layout after the point edit `action` (`set`, `delete` or
    /// `insert`) at `path` — each segment a key or an array index as
    /// decimal digits — whose parent is an array when `in_array`: the
    /// expressions at and below a value set or deleted go, and those after
    /// an item deleted from or inserted into an array move with it.
    ///
    /// # Errors
    ///
    /// [`Error::Format`] when `path` runs through a value read from an
    /// expression: what it holds is the expression's to compute, and no
    /// value dq could write there would stay that expression.
    pub(crate) fn after_edit(&self, action: &str, path: &[String], in_array: bool) -> Result<Self> {
        if let Some((at, expr)) = self
            .exprs
            .iter()
            .filter(|(at, _)| at.len() < path.len() && path.starts_with(at))
            .min_by_key(|(at, _)| at.len())
        {
            return Err(Error::Format {
                format: "hcl",
                message: format!(
                    "cannot {action} {}: {} is the expression `{}`, whose value HCL computes; \
                     dq can replace the whole expression, not edit inside it",
                    pointer_text(path),
                    pointer_text(at),
                    expr.source,
                ),
            });
        }
        let shift = match (in_array, path.split_last()) {
            (true, Some((last, parent))) => last.parse::<usize>().ok().map(|i| (parent, i)),
            _ => None,
        };
        // Where the note at `at` is after the edit: gone with a value set or
        // deleted, moved with an array item shifted.
        let after = |at: &Vec<String>| -> Option<Vec<String>> {
            if action != "insert" && at.starts_with(path) {
                return None;
            }
            let mut at = at.clone();
            if let Some((parent, i)) = shift
                && at.len() > parent.len()
                && at.starts_with(parent)
                && let Ok(j) = at[parent.len()].parse::<usize>()
            {
                match action {
                    "delete" if j > i => at[parent.len()] = (j - 1).to_string(),
                    "insert" if j >= i => at[parent.len()] = (j + 1).to_string(),
                    _ => {}
                }
            }
            Some(at)
        };
        Ok(Self {
            blocks: self.blocks.clone(),
            exprs: self
                .exprs
                .iter()
                .filter_map(|(at, expr)| Some((after(at)?, expr.clone())))
                .collect(),
            keys: self
                .keys
                .iter()
                .filter_map(|(at, source)| Some((after(at)?, source.clone())))
                .collect(),
            moved: false,
        })
    }

    /// Whether `other`, this layout after an edit, notes its expressions and
    /// expression keys elsewhere or not at all: the edit replaced, removed or
    /// moved one.
    pub(crate) fn notes_differ(&self, other: &Self) -> bool {
        self.exprs != other.exprs || self.keys != other.keys
    }
}

/// `path` as a JSON Pointer, for a message.
fn pointer_text(path: &[String]) -> String {
    let pointer = crate::pointer::Pointer::new(
        path.iter()
            .map(|key| crate::pointer::Segment::Key(key.clone()))
            .collect(),
    )
    .as_canonical();
    format!("'{pointer}'")
}

/// Whether a string read from HCL holds what an HCL template would read
/// as an interpolation or a directive, once written as a string.
fn has_template_marker(s: &str) -> bool {
    s.contains("${") || s.contains("%{")
}

/// How a key of one body was read.
#[derive(Clone, Copy)]
enum Slot {
    Attribute,
    /// Blocks carrying this many labels.
    Block(usize),
}

/// One read of an HCL body tree into a [`Value`].
struct Reader<'a> {
    /// The source, for the line and column of an error.
    text: &'a str,
    /// The keys from the root down to the body being read, as
    /// [`HclLayout::blocks`] addresses them.
    path: Vec<String>,
    layout: HclLayout,
}

/// What the reader notes of an expression: what it read as and its source
/// ([`HclLayout::exprs`]), or the source of an object key
/// ([`HclLayout::keys`]).
#[derive(Debug, Clone)]
enum Note {
    Expr(Expr),
    Key(String),
}

/// The expressions of a body, each by its path from that body.
type BodyExprs = Vec<(Vec<String>, Note)>;

impl Reader<'_> {
    /// Convert a body (top-level or nested block body), as `hcl-edit` reads
    /// it, into our `Value`.
    ///
    /// Each attribute in the body becomes a `key → value` entry in the
    /// result map: its expression is turned into `hcl-rs` types as
    /// `hcl::parse` would (`From<hcl_edit::expr::Expression>`), then into a
    /// `Value`, and every number literal that `hcl-rs` misreads is put back
    /// ([`restore_in_expression`]). Each block is grouped under its
    /// identifier, one level of nesting per label (per the `HCL backend
    /// block parses to nested map` spec scenario); blocks repeated with the
    /// same type and labels become a list of their bodies
    /// ([`Reader::insert_block`]).
    ///
    /// # Errors
    ///
    /// [`Error::Parse`] from [`check_expression_keys`], and for a body
    /// whose keys one map cannot hold apart: an attribute and a block of
    /// one name, or blocks of one type with different label counts.
    ///
    /// Returns the body's value, and its expressions ([`HclLayout::exprs`])
    /// by their paths from the body.
    fn body_to_value(&mut self, body: &EditBody) -> Result<(Value, BodyExprs)> {
        let mut out: IndexMap<String, Value> = IndexMap::new();
        let mut slots: HashMap<String, Slot> = HashMap::new();
        let mut exprs = BodyExprs::new();
        // The expressions of each block body, by the block's type and
        // labels, one entry per repeat: a repeated block's bodies become
        // a list, and the path of each expression takes its index.
        let mut block_exprs: IndexMap<Vec<String>, Vec<BodyExprs>> = IndexMap::new();
        for structure in body.iter() {
            match structure {
                EditStructure::Attribute(attr) => {
                    let key = attr.key.as_str().to_owned();
                    // `hcl-edit` refuses an attribute defined twice.
                    if let Some(Slot::Block(_)) = slots.get(&key) {
                        return Err(self.clash(
                            attr.span(),
                            &format!(
                                "attribute {key:?} has the name of a block of the same body; \
                                 dq reads both as the key {key:?} and would lose one"
                            ),
                        ));
                    }
                    let expr = Expression::from(attr.value.clone());
                    check_expression_keys(&expr)?;
                    let mut val = hcl_value_to_value(HclValue::from(expr));
                    restore_in_expression(&attr.value, &mut val);
                    collect_exprs(
                        self.text,
                        &attr.value,
                        &val,
                        &mut vec![key.clone()],
                        &mut exprs,
                    );
                    slots.insert(key.clone(), Slot::Attribute);
                    out.insert(key, val);
                }
                EditStructure::Block(block) => {
                    let (at, body_exprs) = self.insert_block(&mut out, &mut slots, block)?;
                    block_exprs.entry(at).or_default().push(body_exprs);
                }
            }
        }
        for (at, bodies) in block_exprs {
            let repeated = bodies.len() > 1;
            for (index, body) in bodies.into_iter().enumerate() {
                for (rest, expr) in body {
                    let mut path = at.clone();
                    if repeated {
                        path.push(index.to_string());
                    }
                    path.extend(rest);
                    exprs.push((path, expr));
                }
            }
        }
        Ok((Value::Map(out), exprs))
    }

    /// Insert a block into `parent` honouring the labels-as-keys nesting
    /// rule.
    ///
    /// `block_type "a" "b" {}` becomes `parent[block_type] → Map[a] →
    /// Map[b] → body_value`. A block whose type and labels an earlier block
    /// of this body had turns that body into a list of both, in source
    /// order; later repeats are appended. A body is always a map, so a list
    /// at that place is always a list of repeated blocks.
    ///
    /// # Errors
    ///
    /// [`Error::Parse`] when the block's type is an attribute of this body,
    /// or a type earlier blocks of this body carried a different number of
    /// labels for: a map level cannot hold both labels and a body.
    ///
    /// Returns the block's type and labels, and the expressions of its
    /// body by their paths from the body.
    fn insert_block(
        &mut self,
        parent: &mut IndexMap<String, Value>,
        slots: &mut HashMap<String, Slot>,
        block: &EditBlock,
    ) -> Result<(Vec<String>, BodyExprs)> {
        let identifier = block.ident.as_str().to_owned();
        let labels: Vec<String> = block
            .labels
            .iter()
            .map(|l| match l {
                EditLabel::Ident(id) => id.as_str().to_owned(),
                EditLabel::String(s) => s.as_str().to_owned(),
            })
            .collect();
        match slots.get(&identifier) {
            Some(Slot::Attribute) => {
                return Err(self.clash(
                    block.span(),
                    &format!(
                        "block {identifier:?} has the name of an attribute of the same body; \
                         dq reads both as the key {identifier:?} and would lose one"
                    ),
                ));
            }
            Some(Slot::Block(count)) if *count != labels.len() => {
                return Err(self.clash(
                    block.span(),
                    &format!(
                        "block {identifier:?} has {} here and {} on an earlier {identifier:?} \
                         block of the same body; dq nests one map level per label and would \
                         lose one",
                        label_count(labels.len()),
                        label_count(*count),
                    ),
                ));
            }
            _ => {}
        }
        slots.insert(identifier.clone(), Slot::Block(labels.len()));

        let depth = self.path.len();
        self.path.push(identifier.clone());
        self.layout.record(&self.path, labels.len());
        self.path.extend(labels.iter().cloned());
        let body = self.body_to_value(&block.body);
        self.path.truncate(depth);
        let (body, exprs) = body?;
        let mut at = Vec::with_capacity(labels.len() + 1);
        at.push(identifier.clone());
        at.extend(labels.iter().cloned());

        // Walk into `parent[identifier]`, then one map per label but the
        // last: label maps hold nothing but labels, as the slot check above
        // keeps every block of this type at one label count.
        let Some((last, inner)) = labels.split_last() else {
            add_repeat(parent, identifier, body);
            return Ok((at, exprs));
        };
        let mut map = label_map(parent, identifier);
        for label in inner {
            map = label_map(map, label.clone());
        }
        add_repeat(map, last.clone(), body);
        Ok((at, exprs))
    }

    /// An [`Error::Parse`] at the start of `span`.
    fn clash(&self, span: Option<std::ops::Range<usize>>, message: &str) -> Error {
        let span = span.unwrap_or(0..0);
        let (line, col) = line_col(self.text, span.start);
        Error::Parse {
            file: None,
            line,
            col,
            span,
            snippet: String::new(),
            message: format!("HCL error in line {line}, column {col}: {message}"),
        }
    }
}

/// `1 label`, `0 labels`.
fn label_count(count: usize) -> String {
    if count == 1 {
        "1 label".to_owned()
    } else {
        format!("{count} labels")
    }
}

/// The 1-based line and column of byte `offset` of `text`, the column
/// counted in characters.
fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(col).unwrap_or(u32::MAX),
    )
}

/// The map of labels at `parent[key]`, made if it is not there yet.
fn label_map(parent: &mut IndexMap<String, Value>, key: String) -> &mut IndexMap<String, Value> {
    match parent
        .entry(key)
        .or_insert_with(|| Value::Map(IndexMap::new()))
    {
        Value::Map(map) => map,
        other => unreachable!("a label level holds only labels, not {other:?}"),
    }
}

/// Put the block body `body` at `parent[key]`: alone if no block got there
/// first, else after the bodies there, as a list.
fn add_repeat(parent: &mut IndexMap<String, Value>, key: String, body: Value) {
    match parent.get_mut(&key) {
        None => {
            parent.insert(key, body);
        }
        Some(Value::Array(bodies)) => bodies.push(body),
        Some(first) => {
            let first = std::mem::replace(first, Value::Null);
            parent.insert(key, Value::Array(vec![first, body]));
        }
    }
}

/// Fail on an object whose keys repeat once they are the strings a
/// [`Value::Map`] holds: `hcl-rs` rejects `{ a = 1, a = 2 }`, but `a` and
/// `"a"` (or `1` and `"1"`) are two keys to it and one to the map, where
/// the last would silently win.
///
/// # Errors
///
/// [`Error::Parse`] naming the repeated key; `hcl-rs` gives no position.
fn check_expression_keys(expr: &Expression) -> Result<()> {
    match expr {
        Expression::Array(items) => items.iter().try_for_each(check_expression_keys),
        Expression::Parenthesis(inner) => check_expression_keys(inner),
        Expression::Object(object) => {
            let mut seen = std::collections::HashSet::with_capacity(object.len());
            for (key, value) in object.iter() {
                let key = key.to_string();
                if !seen.insert(key.clone()) {
                    return Err(Error::Parse {
                        file: None,
                        line: 0,
                        col: 0,
                        span: 0..0,
                        snippet: String::new(),
                        message: format!(
                            "invalid object item; expected unique object key; found redefined object key {key:?}"
                        ),
                    });
                }
                check_expression_keys(value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Note in `out` every value of `expr` (read as `value`, at `path` from
/// its body) that a value cannot spell, with its source: an expression
/// other than a literal, an array or an object of literal keys — and a
/// string holding `${` or `%{`, which the writer would escape. The items
/// of an array and the members of an object are looked into. An object
/// with a key that is an expression is noted whole, so a point edit inside
/// it is refused, and each such key's source is noted by its member's
/// path; its members are looked into as well, for a transform that edits
/// the object to write each key and member that stays as it was.
fn collect_exprs(
    text: &str,
    expr: &hcl::edit::expr::Expression,
    value: &Value,
    path: &mut Vec<String>,
    out: &mut BodyExprs,
) {
    use hcl::edit::expr::{Expression as EditExpression, ObjectKey};
    match (expr, value) {
        (EditExpression::Null(_) | EditExpression::Bool(_) | EditExpression::Number(_), _) => {}
        (EditExpression::String(_), Value::String(s)) if !has_template_marker(s) => {}
        (EditExpression::Array(items), Value::Array(values)) if items.len() == values.len() => {
            for (index, (item, value)) in items.iter().zip(values).enumerate() {
                path.push(index.to_string());
                collect_exprs(text, item, value, path, out);
                path.pop();
            }
        }
        (EditExpression::Object(object), Value::Map(map))
            if object.len() == map.len()
                && object.iter().all(|(key, _)| {
                    matches!(
                        key,
                        ObjectKey::Ident(_) | ObjectKey::Expression(EditExpression::String(_))
                    )
                }) =>
        {
            for ((_, member), (key, value)) in object.iter().zip(map) {
                path.push(key.clone());
                collect_exprs(text, member.expr(), value, path, out);
                path.pop();
            }
        }
        (EditExpression::Object(object), Value::Map(map)) if object.len() == map.len() => {
            note_whole(text, expr, value, path, out);
            for ((object_key, member), (key, value)) in object.iter().zip(map) {
                path.push(key.clone());
                // A number key reads as the same key written as a string.
                if let ObjectKey::Expression(key_expr) = object_key
                    && !matches!(
                        key_expr,
                        EditExpression::String(_) | EditExpression::Number(_)
                    )
                    && let Some(source) = object_key.span().and_then(|span| text.get(span))
                {
                    out.push((path.clone(), Note::Key(source.to_owned())));
                }
                collect_exprs(text, member.expr(), value, path, out);
                path.pop();
            }
        }
        _ => note_whole(text, expr, value, path, out),
    }
}

/// Note in `out` the expression `expr`, read as `value` at `path`, whole.
fn note_whole(
    text: &str,
    expr: &hcl::edit::expr::Expression,
    value: &Value,
    path: &[String],
    out: &mut BodyExprs,
) {
    if let Some(source) = expr.span().and_then(|span| text.get(span)) {
        out.push((
            path.to_vec(),
            Note::Expr(Expr {
                value: value.clone(),
                source: source.to_owned(),
            }),
        ));
    }
}

fn hcl_value_to_value(v: HclValue) -> Value {
    match v {
        HclValue::Null => Value::Null,
        HclValue::Bool(b) => Value::Bool(b),
        HclValue::Number(n) => hcl_number_to_value(&n),
        HclValue::String(s) => Value::String(s),
        HclValue::Array(items) => Value::Array(items.into_iter().map(hcl_value_to_value).collect()),
        HclValue::Object(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, hcl_value_to_value(v));
            }
            Value::Map(out)
        }
    }
}

/// Put back the exact value of every number literal in `expr` that
/// `hcl-rs` read, into `value`, as the `f64` nearest it: an integer past
/// 64 bits (`18446744073709551617`, `-9223372036854775809`) or a decimal
/// with more digits than an `f64` holds — and the float-ness of a whole
/// float literal (`3.0`, `1e16`), which `hcl-rs` reads as an integer.
/// `hcl-rs` keeps no literal; `hcl-edit`, which `value` was read with,
/// does.
fn restore_in_expression(expr: &hcl::edit::expr::Expression, value: &mut Value) {
    use hcl::edit::expr::Expression as EditExpression;
    match (expr, value) {
        (EditExpression::Number(number), value @ (Value::Float(_) | Value::Int(_))) => {
            let Some(repr) = number.as_repr() else {
                return;
            };
            // An integer literal that fits `i64` is what `hcl-rs` read.
            if matches!(value, Value::Int(_))
                && repr
                    .strip_prefix('-')
                    .unwrap_or(repr)
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            {
                return;
            }
            // `- 5` is a number too.
            let literal: std::borrow::Cow<'_, str> = if repr.contains(char::is_whitespace) {
                repr.chars().filter(|c| !c.is_whitespace()).collect()
            } else {
                std::borrow::Cow::Borrowed(repr)
            };
            // Only the number `hcl-rs` read from this very literal, which
            // reads a whole float (`3.0`, `-0.0`) as an integer.
            let Ok(parsed) = literal.parse::<f64>() else {
                return;
            };
            let read_from_literal = match value {
                Value::Float(f) => parsed.to_bits() == f.to_bits(),
                Value::Int(i) => parsed == *i as f64,
                _ => false,
            };
            if !read_from_literal {
                return;
            }
            match Value::from_number_literal(&literal) {
                Some(Value::Int(_)) | None => {}
                Some(exact) => *value = exact,
            }
        }
        (EditExpression::Array(items), Value::Array(values)) if items.len() == values.len() => {
            for (item, value) in items.iter().zip(values) {
                restore_in_expression(item, value);
            }
        }
        (EditExpression::Object(object), Value::Map(map)) if object.len() == map.len() => {
            for ((_, member), value) in object.iter().zip(map.values_mut()) {
                restore_in_expression(member.expr(), value);
            }
        }
        (EditExpression::Parenthesis(inner), value) => restore_in_expression(inner.inner(), value),
        _ => {}
    }
}

/// Map an `hcl_primitives::Number` into our `Value` numeric variants.
///
/// Integer-shaped numbers go to `Value::Int` when they fit `i64`, otherwise
/// `Value::BigInt(stringified)`. Float-shaped numbers go to `Value::Float`;
/// non-finite floats are not produced by the parser (HCL grammar rejects
/// them) so we don't need a `BigFloat` fallback here.
fn hcl_number_to_value(n: &Number) -> Value {
    if let Some(i) = n.as_i64() {
        return Value::Int(i);
    }
    if n.is_u64()
        && let Some(u) = n.as_u64()
    {
        // u64 that doesn't fit i64 — preserve as BigInt literal so callers
        // don't lose precision.
        return Value::BigInt(u.to_string());
    }
    if let Some(f) = n.as_f64() {
        return Value::Float(f);
    }
    // Fallback: stringify the number's textual form. The `Number` type
    // implements Display so this is non-lossy in practice.
    Value::BigFloat(n.to_string())
}

fn serialize_error(e: hcl::Error) -> Error {
    Error::Format {
        format: "hcl",
        message: format!("failed to serialize HCL: {e}"),
    }
}

/// `map` as an HCL body, laid out as `hcl::to_string` lays out an
/// `hcl::Value` — two-space indents, one array item or object member per
/// line, object keys quoted — with each float spelled by
/// [`shortest_float`](crate::document::shortest_float) and strings and keys
/// quoted by `hcl-rs` itself. Each key `blocks` says the reader read as a
/// block type is written as blocks, `type "label" { … }`, one per body —
/// a list of two or more bodies is a block repeated — as long as its value
/// still has that shape; every other key is an attribute.
///
/// Written here rather than by `hcl-rs` because it writes a whole `f64`
/// that fits 64 bits as an integer (`3.0` → `3`), which reads back as an
/// int, and keeps objects in a map whose every insert scans the keys
/// before it, which takes minutes for an object of 100,000 members.
///
/// # Errors
///
/// [`Error::Format`] naming the first big number, NaN or infinity HCL
/// cannot hold, else the first key that is no HCL identifier.
fn write_body(map: &IndexMap<String, Value>, layout: Option<&HclLayout>) -> Result<String> {
    // Where a transform may have moved values, an expression is also
    // looked for by the string it read as: each such string, with the one
    // source it was read from, or `None` when two sources read as it.
    let mut by_string: HashMap<&str, Option<&str>> = HashMap::new();
    // The same for an object key read from an expression, by the string
    // it read as.
    let mut by_key: HashMap<&str, Option<&str>> = HashMap::new();
    // The strings two sources read as that are no longer each at a place
    // one of them was read at.
    let mut unsettled = std::collections::HashSet::new();
    if let Some(layout) = layout.filter(|layout| layout.moved) {
        let one_source = |seen: &mut Option<&str>, source: &str| {
            if *seen != Some(source) {
                *seen = None;
            }
        };
        for expr in layout.exprs.values() {
            if let Value::String(s) = &expr.value
                && has_template_marker(s)
            {
                by_string
                    .entry(s.as_str())
                    .and_modify(|seen| one_source(seen, &expr.source))
                    .or_insert(Some(expr.source.as_str()));
            }
        }
        for (at, source) in &layout.keys {
            if let Some(key) = at.last() {
                by_key
                    .entry(key.as_str())
                    .and_modify(|seen| one_source(seen, source))
                    .or_insert(Some(source.as_str()));
            }
        }
        unsettled = unsettled_strings(map, layout, &by_string);
    }
    let mut writer = BodyWriter {
        out: String::new(),
        layout,
        by_string,
        by_key,
        unsettled,
        path: Vec::new(),
        exact: Vec::new(),
        bad_key: None,
    };
    writer.body(map, 0)?;
    match writer.bad_key {
        Some(e) => Err(serialize_error(e)),
        None => Ok(writer.out),
    }
}

/// Of the strings more than one source read as (`None` in `by_string`),
/// those a transform may have moved: the places `map` holds one at are not
/// the very places the layout read one at. Where they are, each holds what
/// was read there — or two equal strings swapped, which reads as the same
/// file — and the source read at a place is the one to write there.
fn unsettled_strings<'a>(
    map: &IndexMap<String, Value>,
    layout: &HclLayout,
    by_string: &HashMap<&'a str, Option<&'a str>>,
) -> std::collections::HashSet<&'a str> {
    let mut read_at: HashMap<&str, Vec<&[String]>> = HashMap::new();
    for (at, expr) in &layout.exprs {
        if let Value::String(s) = &expr.value
            && let Some((&key, None)) = by_string.get_key_value(s.as_str())
        {
            read_at.entry(key).or_default().push(at.as_slice());
        }
    }
    if read_at.is_empty() {
        return std::collections::HashSet::new();
    }
    let mut now_at: HashMap<&str, Vec<Vec<String>>> = HashMap::new();
    let mut path = Vec::new();
    for (key, value) in map {
        path.push(key.clone());
        strings_at(value, &mut path, &read_at, &mut now_at);
        path.pop();
    }
    read_at
        .into_iter()
        .filter(|(s, read)| {
            let mut read: Vec<&[String]> = read.clone();
            let mut now: Vec<&[String]> = now_at
                .get(s)
                .map(|places| places.iter().map(Vec::as_slice).collect())
                .unwrap_or_default();
            read.sort_unstable();
            now.sort_unstable();
            read != now
        })
        .map(|(s, _)| s)
        .collect()
}

/// Note in `now_at` the path of every string of `value` (at `path`) that
/// `read_at` has.
fn strings_at<'a>(
    value: &Value,
    path: &mut Vec<String>,
    read_at: &HashMap<&'a str, Vec<&[String]>>,
    now_at: &mut HashMap<&'a str, Vec<Vec<String>>>,
) {
    match value {
        Value::String(s) => {
            if let Some((&key, _)) = read_at.get_key_value(s.as_str()) {
                now_at.entry(key).or_default().push(path.clone());
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                strings_at(item, path, read_at, now_at);
                path.pop();
            }
        }
        Value::Map(members) => {
            for (key, member) in members {
                path.push(key.clone());
                strings_at(member, path, read_at, now_at);
                path.pop();
            }
        }
        _ => {}
    }
}

/// One write of a body tree (see [`write_body`]).
struct BodyWriter<'a> {
    out: String,
    layout: Option<&'a HclLayout>,
    /// See [`write_body`].
    by_string: HashMap<&'a str, Option<&'a str>>,
    /// See [`write_body`].
    by_key: HashMap<&'a str, Option<&'a str>>,
    /// See [`unsettled_strings`].
    unsettled: std::collections::HashSet<&'a str>,
    /// The keys from the root down to the key being written, as
    /// [`HclLayout::blocks`] addresses them.
    path: Vec<String>,
    /// The path from the root of the value being written, as
    /// [`HclLayout::exprs`] addresses it.
    exact: Vec<String>,
    /// The first key that is no HCL identifier, reported once every number
    /// has been checked.
    bad_key: Option<hcl::Error>,
}

impl<'a> BodyWriter<'a> {
    /// Append the attributes and blocks of `map`, `depth` indents deep.
    fn body(&mut self, map: &IndexMap<String, Value>, depth: usize) -> Result<()> {
        for (key, value) in map {
            self.path.push(key.clone());
            self.exact.push(key.clone());
            let written = match self.layout.and_then(|l| l.labels_at(&self.path)) {
                Some(labels) if has_block_shape(value, labels) => {
                    self.blocks_of(key, value, labels, &mut Vec::new(), depth)
                }
                _ => self.attribute(key, value, depth),
            };
            self.exact.pop();
            self.path.pop();
            written?;
        }
        Ok(())
    }

    /// Append `key = value`.
    fn attribute(&mut self, key: &str, value: &Value, depth: usize) -> Result<()> {
        indent(&mut self.out, depth);
        self.identifier(key);
        self.out.push_str(" = ");
        self.expression(value, depth + 1)?;
        self.out.push('\n');
        Ok(())
    }

    /// Append the blocks of type `key` that `value` holds below `labels`,
    /// `remaining` label levels deep: a body is one block, a list of them
    /// one block each.
    fn blocks_of<'v>(
        &mut self,
        key: &str,
        value: &'v Value,
        remaining: usize,
        labels: &mut Vec<&'v str>,
        depth: usize,
    ) -> Result<()> {
        match value {
            Value::Map(map) if remaining > 0 => {
                for (label, inner) in map {
                    self.path.push(label.clone());
                    self.exact.push(label.clone());
                    labels.push(label);
                    let written = self.blocks_of(key, inner, remaining - 1, labels, depth);
                    labels.pop();
                    self.exact.pop();
                    self.path.pop();
                    written?;
                }
            }
            Value::Map(body) => self.block(key, labels, body, depth)?,
            Value::Array(bodies) => {
                for (index, body) in bodies.iter().enumerate() {
                    if let Value::Map(body) = body {
                        self.exact.push(index.to_string());
                        let written = self.block(key, labels, body, depth);
                        self.exact.pop();
                        written?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Append `key "label" … { body }`.
    fn block(
        &mut self,
        key: &str,
        labels: &[&str],
        body: &IndexMap<String, Value>,
        depth: usize,
    ) -> Result<()> {
        indent(&mut self.out, depth);
        self.identifier(key);
        for label in labels {
            self.out.push(' ');
            self.out
                .push_str(&hcl_text(&hcl::Expression::String((*label).to_owned()))?);
        }
        self.out.push_str(" {\n");
        let start = self.out.len();
        self.body(body, depth + 1)?;
        // A body whose keys all wrote nothing (block types with no body
        // left) is an empty block.
        if self.out.len() == start {
            self.out.truncate(start - " {\n".len());
            self.out.push_str(" {}\n");
            return Ok(());
        }
        indent(&mut self.out, depth);
        self.out.push_str("}\n");
        Ok(())
    }

    /// Append `key` if it is an HCL identifier, else remember it.
    fn identifier(&mut self, key: &str) {
        match hcl::Identifier::new(key) {
            Ok(ident) => self.out.push_str(ident.as_str()),
            Err(e) => {
                self.bad_key.get_or_insert(e);
            }
        }
    }

    /// The source to write for `value`, at [`BodyWriter::exact`], if it is
    /// an expression the reader saw: the one read there, when `value` is
    /// still what it read as — unless a transform may have moved it, and
    /// another source read as it too — else, where a transform may have
    /// moved values, the one the string `value` was read from anywhere.
    ///
    /// # Errors
    ///
    /// [`Error::Format`] when the string `value` holds `${` or `%{` and is
    /// what two expressions of different source read as, and a transform
    /// may have moved it, or replaces an expression it does not read as:
    /// written as a string it would turn what was an expression into a
    /// string, and dq cannot tell an edited expression from a new string.
    fn source_of(&self, value: &Value) -> Result<Option<&'a str>> {
        let Some(layout) = self.layout else {
            return Ok(None);
        };
        let here = layout.exprs.get(&self.exact);
        if let Some(expr) = here
            && expr.value.same_data(value)
            && !matches!(value, Value::String(s) if self.unsettled.contains(s.as_str()))
        {
            return Ok(Some(expr.source.as_str()));
        }
        let Value::String(s) = value else {
            return Ok(None);
        };
        if !layout.moved || !has_template_marker(s) {
            return Ok(None);
        }
        match (self.by_string.get(s.as_str()), here) {
            (Some(Some(source)), _) => Ok(Some(source)),
            (Some(None), _) => Err(Error::Format {
                format: "hcl",
                message: format!(
                    "{}: the string {s:?} is what more than one expression of the file reads \
                     as, and the edit moved it; dq cannot tell which expression to write there",
                    pointer_text(&self.exact),
                ),
            }),
            (None, Some(expr)) => Err(Error::Format {
                format: "hcl",
                message: format!(
                    "{}: the edit turned the expression `{}` into the string {s:?}; dq would \
                     write that as a string, no longer an expression, so it refuses: edit the \
                     expression in the file itself, or set a value that holds no `${{` or `%{{`",
                    pointer_text(&self.exact),
                    expr.source,
                ),
            }),
            (None, None) => Ok(None),
        }
    }

    /// Append the object key `key`, at [`BodyWriter::exact`] and a member
    /// of it: the source of the expression key read there, else — where a
    /// transform may have moved values — the one expression key read as
    /// `key` anywhere, else `key` as a string.
    ///
    /// # Errors
    ///
    /// [`Error::Format`] when a transform may have moved the key, and
    /// expression keys of different source read as it: written as a string
    /// it would be a literal key, no longer the expression.
    fn object_key(&mut self, key: &str) -> Result<()> {
        if let Some(layout) = self.layout {
            self.exact.push(key.to_owned());
            let here = layout.keys.get(&self.exact);
            self.exact.pop();
            let source = match (here, layout.moved, self.by_key.get(key)) {
                (Some(source), _, _) => Some(source.as_str()),
                (None, true, Some(Some(source))) => Some(*source),
                (None, true, Some(None)) => {
                    return Err(Error::Format {
                        format: "hcl",
                        message: format!(
                            "{}: the key {key:?} is what more than one expression key of the \
                             file reads as, and the edit moved it; dq cannot tell which \
                             expression to write there",
                            pointer_text(&self.exact),
                        ),
                    });
                }
                _ => None,
            };
            if let Some(source) = source {
                self.out.push_str(source);
                return Ok(());
            }
        }
        self.out.push_str(&hcl_text(&hcl::ObjectKey::from(key))?);
        Ok(())
    }

    /// Append `value`, whose members go `depth` indents deep: the source
    /// of the expression it was read from ([`BodyWriter::source_of`]), else
    /// the value.
    ///
    /// # Errors
    ///
    /// [`Error::Format`] from [`BodyWriter::source_of`], and naming the
    /// first big number, NaN or infinity of `value`: HCL has no literal for
    /// any of them.
    fn expression(&mut self, value: &Value, depth: usize) -> Result<()> {
        if let Some(source) = self.source_of(value)? {
            self.out.push_str(source);
            return Ok(());
        }
        // A NaN or an infinity, held as a float or as its literal (`.nan`
        // read from YAML): HCL has no literal for one, so writing `null` or
        // the string `".nan"` would change the value without a word.
        if let Some(spelling) = value.non_finite_spelling() {
            return Err(non_finite_error(spelling));
        }
        match value {
            Value::Null => self.out.push_str("null"),
            Value::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => self.out.push_str(&i.to_string()),
            Value::Float(f) => self.out.push_str(&crate::document::shortest_float(*f)),
            Value::BigInt(s) => match s.parse::<u64>() {
                Ok(u) if u.to_string() == *s => self.out.push_str(s),
                _ => return Err(big_number_error(s)),
            },
            Value::BigFloat(s) => return Err(big_number_error(s)),
            Value::String(s) => self
                .out
                .push_str(&hcl_text(&hcl::Expression::String(s.clone()))?),
            Value::Array(items) if items.is_empty() => self.out.push_str("[]"),
            Value::Array(items) => {
                self.out.push('[');
                // A heredoc's closing marker stands on a line of its own,
                // so the comma after one goes on the next line.
                let mut after_heredoc = false;
                for (at, item) in items.iter().enumerate() {
                    if after_heredoc {
                        self.out.push('\n');
                        indent(&mut self.out, depth);
                    }
                    self.out.push_str(if at == 0 { "\n" } else { ",\n" });
                    indent(&mut self.out, depth);
                    self.exact.push(at.to_string());
                    let start = self.out.len();
                    let written = self.expression(item, depth + 1);
                    self.exact.pop();
                    written?;
                    after_heredoc = self.out[start..].starts_with("<<");
                }
                self.out.push('\n');
                indent(&mut self.out, depth - 1);
                self.out.push(']');
            }
            Value::Map(map) if map.is_empty() => self.out.push_str("{}"),
            Value::Map(map) => {
                self.out.push('{');
                for (key, member) in map {
                    self.out.push('\n');
                    indent(&mut self.out, depth);
                    self.object_key(key)?;
                    self.out.push_str(" = ");
                    self.exact.push(key.clone());
                    let written = self.expression(member, depth + 1);
                    self.exact.pop();
                    written?;
                }
                self.out.push('\n');
                indent(&mut self.out, depth - 1);
                self.out.push('}');
            }
        }
        Ok(())
    }
}

/// Whether `value` can be written as the blocks of a type with `labels`
/// labels: `labels` levels of maps, then a body (a map) or a list of
/// bodies. A key read as a block stays one with any number of bodies left:
/// a list of one is written as one block, which reads back as the body
/// alone, and an empty list or label map as no block, which removes the
/// key.
fn has_block_shape(value: &Value, labels: usize) -> bool {
    match value {
        Value::Map(map) if labels > 0 => map.values().all(|v| has_block_shape(v, labels - 1)),
        Value::Map(_) => true,
        Value::Array(bodies) if labels == 0 => bodies.iter().all(|b| matches!(b, Value::Map(_))),
        _ => false,
    }
}

/// Append `depth` two-space indents to `out`.
fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// `value` as `hcl-rs` formats it.
fn hcl_text<T: hcl::format::Format>(value: &T) -> Result<String> {
    hcl::format::to_string(value).map_err(serialize_error)
}

/// Why `value`, which is no map, cannot be the root of an HCL document: a
/// file is a body of attributes and blocks, which only a map holds.
/// `hcl-rs` would refuse it in words that change with the value's type
/// (`serialize_u64 not supported`).
fn hcl_root_refusal(value: &Value) -> String {
    format!(
        "an HCL document is a body of attributes, so its root is a map, not {}",
        value.kind_with_article()
    )
}

/// HCL numbers are `u64` / `i64` / `f64`.
fn big_number_error(literal: &str) -> Error {
    Error::Format {
        format: "hcl",
        message: format!("HCL cannot represent arbitrary-precision number {literal}"),
    }
}

/// HCL has no literal for a NaN or an infinity. `spelling` is the one dq
/// gives it everywhere (`.nan`, `.inf`, `-.inf`).
fn non_finite_error(spelling: &str) -> Error {
    Error::Format {
        format: "hcl",
        message: format!("HCL cannot represent non-finite number {spelling}"),
    }
}

/// Map an `hcl::Error` into our structured `Error::Parse`.
///
/// The `Parse` variant exposes `Location { line, column }`; other error
/// variants don't carry position info, so we anchor at `0/0` for them.
fn map_hcl_parse_error(e: hcl::Error) -> Error {
    let (line, col, message) = match &e {
        hcl::Error::Parse(parse_err) => {
            let loc = parse_err.location();
            (loc.line() as u32, loc.column() as u32, e.to_string())
        }
        other => (0, 0, other.to_string()),
    };
    Error::Parse {
        file: None,
        line,
        col,
        span: 0..0,
        snippet: String::new(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convert our `Value` back into `hcl::Value`: the oracle [`write_body`]
    /// lays its output out against, `hcl-rs`'s own writer.
    ///
    /// A [`Value::BigInt`] that fits `u64` becomes that number. Any other
    /// finite big number (`Value::BigInt` / `Value::BigFloat`) has no `hcl-rs`
    /// `Number` — written as a string or the nearest `f64` it would come back
    /// another value — and fails, and so does a NaN or an infinity.
    ///
    /// # Errors
    ///
    /// [`Error::Format`] naming the first big number HCL cannot hold.
    fn value_to_hcl_value(v: &Value) -> Result<HclValue> {
        Ok(match v {
            Value::Null => HclValue::Null,
            Value::Bool(b) => HclValue::Bool(*b),
            Value::Int(n) => HclValue::Number(Number::from(*n)),
            Value::Float(n) => match Number::from_f64(*n) {
                Some(n) => HclValue::Number(n),
                None => return Err(non_finite_error(v.non_finite_spelling().unwrap_or("NaN"))),
            },
            Value::BigInt(s) => match s.parse::<u64>() {
                Ok(u) if u.to_string() == *s => HclValue::Number(Number::from(u)),
                _ => return Err(big_number_error(s)),
            },
            Value::BigFloat(_) if v.non_finite_float().is_some() => {
                return Err(non_finite_error(v.non_finite_spelling().unwrap_or("NaN")));
            }
            Value::BigFloat(s) => return Err(big_number_error(s)),
            Value::String(s) => HclValue::String(s.clone()),
            Value::Array(items) => HclValue::Array(
                items
                    .iter()
                    .map(value_to_hcl_value)
                    .collect::<Result<_>>()?,
            ),
            Value::Map(map) => {
                let mut out = hcl::Map::with_capacity(map.len());
                for (k, val) in map {
                    out.insert(k.clone(), value_to_hcl_value(val)?);
                }
                HclValue::Object(out)
            }
        })
    }

    #[test]
    fn parse_keeps_numbers_an_f64_misreads_exact() {
        let text = "x = 18446744073709551617\nn = - 9223372036854775809\n\
                    f = 0.10000000000000000001\nl = [1, (18446744073709551616)]\n\
                    o = { k = 18446744073709551618 }\nu = 18446744073709551615\n\
                    h = 1.5\nb \"lbl\" {\n  y = 36893488147419103232\n}\n";
        let doc = Hcl.parse(text.as_bytes()).expect("parse");
        let Value::Map(map) = doc.value() else {
            panic!("{:?}", doc.value());
        };
        let int = |s: &str| Value::BigInt(s.to_owned());
        assert_eq!(map["x"], int("18446744073709551617"));
        assert_eq!(map["n"], int("-9223372036854775809"));
        assert_eq!(map["f"], Value::BigFloat("0.10000000000000000001".into()));
        assert_eq!(
            map["l"],
            Value::Array(vec![Value::Int(1), int("18446744073709551616")])
        );
        assert_eq!(
            map["o"],
            Value::Map(
                [("k".to_owned(), int("18446744073709551618"))]
                    .into_iter()
                    .collect()
            )
        );
        assert_eq!(map["u"], int("18446744073709551615"));
        assert_eq!(map["h"], Value::Float(1.5));
        let Value::Map(b) = &map["b"] else {
            panic!("{:?}", map["b"]);
        };
        let Value::Map(lbl) = &b["lbl"] else {
            panic!("{:?}", b["lbl"]);
        };
        assert_eq!(lbl["y"], int("36893488147419103232"));
        // Each of two blocks of one type keeps its own number, also when
        // the two literals read as the same `f64`, or both are big.
        let one = |x: Value| Value::Map([("x".to_owned(), x)].into_iter().collect());
        for (text, path, first, second) in [
            (
                "b {\n  x = 18446744073709551617\n}\nb {\n  x = 2.5\n}\n",
                &["b"][..],
                one(int("18446744073709551617")),
                one(Value::Float(2.5)),
            ),
            (
                "b {\n x = 18446744073709551617\n}\nb {\n x = 18446744073709551616\n}\n",
                &["b"],
                one(int("18446744073709551617")),
                one(int("18446744073709551616")),
            ),
            (
                "b {x = 18446744073709551617}\nb {x = 1.8446744073709552e19}\n",
                &["b"],
                one(int("18446744073709551617")),
                one(Value::Float(1.844_674_407_370_955_2e19)),
            ),
            (
                "p \"a\" {\n x = 0.10000000000000000001\n}\np \"a\" {\n x = 0.1\n}\n",
                &["p", "a"],
                one(Value::BigFloat("0.10000000000000000001".into())),
                one(Value::Float(0.1)),
            ),
        ] {
            let doc = Hcl.parse(text.as_bytes()).expect("parse");
            let found = path.iter().try_fold(doc.value(), |value, key| match value {
                Value::Map(map) => map.get(*key),
                _ => None,
            });
            assert_eq!(found, Some(&Value::Array(vec![first, second])), "{text}");
        }
    }

    #[test]
    fn parse_fails_on_object_keys_that_repeat_as_strings() {
        for text in [
            "x = { a = 1, \"a\" = 2 }\n",
            "x = [{ 1 = 1, \"1\" = 2 }]\n",
            "b {\n  x = ({ k = 1, \"k\" = 2 })\n}\n",
        ] {
            let err = Hcl.parse(text.as_bytes()).expect_err(text);
            let Error::Parse { message, .. } = err else {
                panic!("{err:?}")
            };
            assert!(message.contains("found redefined object key"), "{message}");
        }
        assert!(Hcl.parse(b"x = { a = 1, \"b\" = 2 }\n").is_ok());
    }

    #[test]
    fn parse_simple_attribute() {
        let doc = Hcl.parse(b"key = \"value\"\n").expect("parse simple");
        match doc.value() {
            Value::Map(m) => assert_eq!(m.get("key"), Some(&Value::String("value".into()))),
            other => panic!("expected map, got: {other:?}"),
        }
    }

    #[test]
    fn parse_block_with_label_nests() {
        let doc = Hcl
            .parse(b"backend \"s3\" { region = \"us-east-1\" }\n")
            .expect("parse block");
        let Value::Map(top) = doc.value() else {
            panic!("expected top map")
        };
        let Some(Value::Map(backend)) = top.get("backend") else {
            panic!("expected backend map")
        };
        let Some(Value::Map(s3)) = backend.get("s3") else {
            panic!("expected s3 map")
        };
        assert_eq!(s3.get("region"), Some(&Value::String("us-east-1".into())));
    }

    #[test]
    fn parse_invalid_returns_parse_error() {
        let err = Hcl.parse(b"this = =\n").expect_err("must parse-error");
        assert_eq!(err.kind_name(), "parse");
    }

    fn write(value: Value) -> Result<String> {
        let mut out = Vec::new();
        Hcl.write(&Document::single(value), &mut out)?;
        Ok(String::from_utf8(out).expect("UTF-8"))
    }

    #[test]
    fn a_whole_float_stays_a_float_written_and_read() {
        let value = Value::Map(
            [
                ("e".to_owned(), Value::Float(3.0)),
                ("n".to_owned(), Value::Float(-3.0)),
                ("h".to_owned(), Value::Float(1e16)),
                ("i".to_owned(), Value::Int(4)),
            ]
            .into_iter()
            .collect(),
        );
        let text = write(value.clone()).expect("writes");
        assert_eq!(text, "e = 3.0\nn = -3.0\nh = 1e16\ni = 4\n");
        let back = Hcl.parse(text.as_bytes()).expect("reads back");
        assert_eq!(back.value(), &value);
    }

    #[test]
    fn write_lays_out_a_body_as_hcl_rs_does() {
        // No floats: `hcl-rs` writes these as dq does.
        let strings = [
            "plain",
            "",
            "quote \" back \\ tab \t nl \n cr \r bell \u{7}",
            "${var.x} templated \"raw\"",
            "$${escaped} and %%{ also }",
            "dollar ${ alone",
            "\u{E000} private use, \u{fc}n\u{ef}code",
        ];
        let map = |members: Vec<(&str, Value)>| {
            Value::Map(
                members
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v))
                    .collect(),
            )
        };
        let value = map(vec![
            (
                "s",
                Value::Array(strings.iter().map(|s| Value::String((*s).into())).collect()),
            ),
            ("e", Value::Array(vec![])),
            ("o", map(vec![])),
            ("n", Value::Null),
            ("b", Value::Bool(true)),
            ("i", Value::Int(-7)),
            ("u", Value::BigInt("18446744073709551615".into())),
            (
                "deep",
                map(vec![
                    (
                        "a b",
                        Value::Array(vec![Value::Array(vec![Value::Int(1)]), map(vec![])]),
                    ),
                    (
                        "${k}",
                        map(vec![(
                            "x",
                            Value::Array(vec![Value::Int(2), Value::Int(3)]),
                        )]),
                    ),
                ]),
            ),
        ]);
        let Value::Map(body) = &value else {
            unreachable!()
        };
        let expected = hcl::to_string(&value_to_hcl_value(&value).unwrap()).unwrap();
        assert_eq!(write_body(body, None).unwrap(), expected);
    }

    #[test]
    fn write_spells_every_float_with_shortest_float() {
        let value = Value::Map(
            [
                ("w".to_owned(), Value::Float(2.0)),
                (
                    "a".to_owned(),
                    Value::Array(vec![
                        Value::Float(1_658_206_780_088_562.2),
                        Value::Map([("k".to_owned(), Value::Float(-0.5))].into_iter().collect()),
                    ]),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let text = write(value).expect("writes");
        assert_eq!(
            text,
            "w = 2.0\na = [\n  1658206780088562.2,\n  {\n    \"k\" = -0.5\n  }\n]\n"
        );
    }

    #[test]
    fn write_refuses_a_root_that_is_no_map() {
        for (value, kind) in [
            (Value::Array(vec![Value::Int(1)]), "an array"),
            (Value::Array(vec![Value::Float(1.5)]), "an array"),
            (Value::String("s".into()), "a string"),
            (Value::BigInt("18446744073709551617".into()), "an integer"),
            (Value::Bool(true), "a boolean"),
            (Value::Null, "null"),
        ] {
            assert_eq!(
                write(value).unwrap_err().to_string(),
                format!(
                    "hcl format error: an HCL document is a body of attributes, so its root is \
                     a map, not {kind}"
                )
            );
        }
    }

    #[test]
    fn write_names_a_big_number_before_a_key_that_is_no_identifier() {
        let value = Value::Map(
            [
                ("a b".to_owned(), Value::Int(1)),
                ("x".to_owned(), Value::BigFloat("1e400".into())),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            write(value).unwrap_err().to_string(),
            "hcl format error: HCL cannot represent arbitrary-precision number 1e400"
        );
        let value = Value::Map([("a b".to_owned(), Value::Int(1))].into_iter().collect());
        assert_eq!(
            write(value).unwrap_err().to_string(),
            "hcl format error: failed to serialize HCL: invalid identifier `a b`"
        );
    }

    #[test]
    fn write_refuses_a_big_number_hcl_cannot_hold() {
        for big in [
            Value::BigInt("18446744073709551616".into()),
            Value::BigInt("-9223372036854775809".into()),
            Value::BigFloat("1e400".into()),
        ] {
            let mut map = indexmap::IndexMap::new();
            map.insert("a".to_owned(), Value::Array(vec![big.clone()]));
            let err = write(Value::Map(map)).expect_err("big number refused");
            assert_eq!(
                err.to_string(),
                format!("hcl format error: HCL cannot represent arbitrary-precision number {big}")
            );
        }
    }

    #[test]
    fn write_refuses_nan_and_infinities_as_float_or_literal() {
        for (value, spelling) in [
            (Value::Float(f64::NAN), ".nan"),
            (Value::Float(f64::INFINITY), ".inf"),
            (Value::Float(f64::NEG_INFINITY), "-.inf"),
            (Value::BigFloat(".nan".into()), ".nan"),
            (Value::BigFloat(".Inf".into()), ".inf"),
            (Value::BigFloat("-.inf".into()), "-.inf"),
        ] {
            let mut map = indexmap::IndexMap::new();
            map.insert("a".to_owned(), Value::Array(vec![value.clone()]));
            let err = write(Value::Map(map)).expect_err("non-finite refused");
            assert_eq!(
                err.to_string(),
                format!("hcl format error: HCL cannot represent non-finite number {spelling}"),
                "{value:?}"
            );
        }
    }

    fn layout_of(text: &str) -> HclLayout {
        match Hcl.parse(text.as_bytes()).expect("parse").layout() {
            Layout::Hcl(layout) => layout.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn path(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| (*s).to_owned()).collect()
    }

    fn sources(layout: &HclLayout) -> Vec<(Vec<String>, String)> {
        let mut found: Vec<(Vec<String>, String)> = layout
            .exprs
            .iter()
            .map(|(at, expr)| (at.clone(), expr.source.clone()))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn the_reader_notes_each_expression_by_its_exact_path() {
        let layout = layout_of(
            "a = var.x\nb = [1, f(2)]\nc = { k = \"$${l}\", n = 1 }\n\
             r \"t\" {\n  e = x.y\n}\nr \"t\" {\n  e = 3\n}\nq {\n  e = z\n}\n",
        );
        assert_eq!(
            sources(&layout),
            vec![
                (path(&["a"]), "var.x".to_owned()),
                (path(&["b", "1"]), "f(2)".to_owned()),
                (path(&["c", "k"]), "\"$${l}\"".to_owned()),
                (path(&["q", "e"]), "z".to_owned()),
                (path(&["r", "t", "0", "e"]), "x.y".to_owned()),
            ]
        );
    }

    #[test]
    fn the_reader_notes_an_expression_key_and_the_object_it_is_in() {
        let layout = layout_of("t = { (var.k) = var.v, 1 = 2, \"s\" = 3, b = 4 }\n");
        assert_eq!(
            sources(&layout),
            vec![
                (
                    path(&["t"]),
                    "{ (var.k) = var.v, 1 = 2, \"s\" = 3, b = 4 }".to_owned()
                ),
                (path(&["t", "(var.k)"]), "var.v".to_owned()),
            ]
        );
        // A number or string key reads as what it writes as.
        let keys: Vec<_> = layout.keys.iter().collect();
        assert_eq!(
            keys,
            vec![(&path(&["t", "(var.k)"]), &"(var.k)".to_owned())]
        );
        // Gone with the object replaced.
        let set = layout.after_edit("set", &path(&["t"]), false).unwrap();
        assert!(set.keys.is_empty() && layout.notes_differ(&set));
        assert!(!layout.notes_differ(&layout.moved()));
    }

    #[test]
    fn after_edit_drops_replaced_expressions_and_moves_shifted_ones() {
        let layout = layout_of("l = [a, b, c]\nm = d\n");
        let deleted = layout
            .after_edit("delete", &path(&["l", "1"]), true)
            .unwrap();
        assert_eq!(
            sources(&deleted),
            vec![
                (path(&["l", "0"]), "a".to_owned()),
                (path(&["l", "1"]), "c".to_owned()),
                (path(&["m"]), "d".to_owned()),
            ]
        );
        let inserted = layout
            .after_edit("insert", &path(&["l", "1"]), true)
            .unwrap();
        assert_eq!(
            sources(&inserted),
            vec![
                (path(&["l", "0"]), "a".to_owned()),
                (path(&["l", "2"]), "b".to_owned()),
                (path(&["l", "3"]), "c".to_owned()),
                (path(&["m"]), "d".to_owned()),
            ]
        );
        let set = layout.after_edit("set", &path(&["m"]), false).unwrap();
        assert_eq!(sources(&set).len(), 3);
        assert!(!set.moved && layout.moved().moved);
        let err = layout
            .after_edit("set", &path(&["m", "k"]), false)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "hcl format error: cannot set '/m/k': '/m' is the expression `d`, whose value HCL \
             computes; dq can replace the whole expression, not edit inside it"
        );
    }

    #[test]
    fn a_block_type_stays_blocks_with_one_body_left_and_goes_with_none() {
        let pointer = |p: &str| crate::pointer::Pointer::parse(p).unwrap();
        let src =
            "r \"sg\" \"app\" {\n  ingress {\n    p = 80\n  }\n  ingress {\n    p = 443\n  }\n}\n";
        let mut doc = Hcl.parse(src.as_bytes()).expect("parse");
        doc.del_at(&pointer("/r/sg/app/ingress/0")).expect("del");
        assert_eq!(
            String::from_utf8(doc.original_bytes().to_vec()).unwrap(),
            "r \"sg\" \"app\" {\n  ingress {\n    p = 443\n  }\n}\n"
        );
        // A transform that leaves one body in a list, or none, or no label.
        let doc = Hcl.parse(src.as_bytes()).expect("parse");
        for (filter, want) in [
            (
                &["r", "sg", "app", "ingress"][..],
                "r \"sg\" \"app\" {\n  ingress {\n    p = 80\n  }\n}\n",
            ),
            (&["r", "sg", "app"], "r \"sg\" \"app\" {}\n"),
            (&["r", "sg"], ""),
        ] {
            let mut tree = doc.value().clone();
            let mut at = &mut tree;
            for key in filter {
                let Value::Map(map) = at else { panic!() };
                at = map.get_mut(*key).unwrap();
            }
            *at = match at {
                Value::Array(bodies) => Value::Array(bodies[..1].to_vec()),
                Value::Map(_) if filter.len() == 3 => {
                    Value::Map([("ingress".to_owned(), Value::Array(vec![]))].into())
                }
                _ => Value::Map(IndexMap::new()),
            };
            let mut out = Vec::new();
            Hcl.write(
                &Document::value_only(tree, FormatTag::Hcl).with_layout_of(&doc),
                &mut out,
            )
            .expect("write");
            assert_eq!(String::from_utf8(out).unwrap(), want, "{filter:?}");
        }
    }

    #[test]
    fn write_keeps_a_big_integer_in_u64_a_number() {
        let mut map = indexmap::IndexMap::new();
        map.insert("a".to_owned(), Value::BigInt("18446744073709551615".into()));
        assert_eq!(
            write(Value::Map(map)).unwrap(),
            "a = 18446744073709551615\n"
        );
    }
}
