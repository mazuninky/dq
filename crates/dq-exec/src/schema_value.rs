//! A [`dq_core::Value`] as a JSON Schema instance.
//!
//! The JSON Schema checks validate the document's own value through
//! jsonschema's public representation API (`jsonschema::json::Json`,
//! documented as the way to validate a value that is not a
//! `serde_json::Value`), not a `serde_json::Value` copy of it. A copy
//! cannot hold a NaN or an infinity: a YAML `.nan` became the string
//! `".nan"`, so `type: number` failed on it and `type: string` passed.
//!
//! What the validator sees:
//!
//! - every number is a number: `type: number` passes on `.nan` / `.inf`;
//!   `type: integer` asks [`dq_core::Value::is_whole_number`], so it holds
//!   for `18446744073709551616` and `1e400` and fails for a NaN, an
//!   infinity and `1e-400`;
//! - the bounds (`minimum`, `maximum`, `exclusiveMinimum`,
//!   `exclusiveMaximum`) compare the instance and the limit by their exact
//!   value ([`dq_core::Value::compare_number`]): `18446744073709551617`
//!   is past `maximum: 18446744073709551616`, `0.1` below
//!   `exclusiveMaximum: 0.10000000000000000001`; an infinity is past every
//!   finite limit on its side (`.inf` fails every maximum and meets every
//!   minimum), and a NaN, unordered as every IEEE comparison with one is
//!   false, fails every bound;
//! - `multipleOf` holds when the instance divided by the divisor is an
//!   integer, by their exact values ([`dq_core::Value::is_multiple_of`]):
//!   `0.3` is a multiple of `0.1`, of `1e400` only `0` is among finite
//!   numbers, and a divisor too small for an `f64` (`1e-400`) compiles
//!   ([`compiled_json`]); a NaN and an infinity are multiples of
//!   nothing;
//! - `enum` and `const` compare numbers by their exact value
//!   ([`dq_core::Value::same_number`]: `1` is `1.0`, a 40-digit integer is
//!   only itself); an infinity equals only the infinity of its sign (which
//!   only a YAML schema can hold), a NaN nothing; under `uniqueItems` two
//!   NaNs are the same item.
//!
//! The schema itself is JSON whose numbers keep their literal (the
//! workspace builds `serde_json` with `arbitrary_precision`). What its JSON
//! cannot hold — a YAML schema's NaN or infinity, a limit's source text
//! (`1e2`) — is a [`SchemaNotes`] beside it, keyed by place in the schema,
//! never a member of it: a member the rule names like one of dq's is the
//! rule's own. Only the copy the validator compiles ([`compiled_json`])
//! holds dq's text in band, under a per-process random tag ([`InBand`]):
//! a NaN or an infinity there is a [`marker`] string, so where a number
//! must stand (`maximum: .inf`) the schema does not compile.
//!
//! The validator renders an error's instance as JSON, which has no NaN;
//! [`instance_json`] spells one as a marker string and [`restore`] puts
//! `.nan` / `.inf` / `-.inf` back into the message.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};

use dq_core::Value;
use indexmap::IndexMap;
use jsonschema::json::{Array, Json, JsonNumber, Node, NodeIdentity, Object};
use jsonschema::types::JsonType;

/// The `dq_core::Value` representation for `jsonschema::options_for`.
#[derive(Debug)]
pub(crate) struct DqJson;

impl Json for DqJson {
    type Node<'a> = DqNode<'a>;
    type PreparedKey = String;
    type StringBuffer = ();

    // A document is a tree: no node is its own descendant.
    const ACYCLIC: bool = true;

    fn prepare_key(key: &str) -> String {
        key.to_owned()
    }

    fn with_string_node<T>((): &mut (), string: &str, f: impl FnOnce(DqNode<'_>) -> T) -> T {
        let node = Value::String(string.to_owned());
        f(DqNode(&node))
    }
}

/// One node of a [`dq_core::Value`] tree.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DqNode<'a>(pub(crate) &'a Value);

/// A map node's members.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DqObject<'a>(&'a IndexMap<String, Value>);

/// An array node's items.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DqArray<'a>(&'a [Value]);

/// A number node: an int, a float (a NaN or an infinity included) or a
/// big-number literal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DqNumber<'a>(&'a Value);

impl<'a> Node<'a, DqJson> for DqNode<'a> {
    type Object = DqObject<'a>;
    type Array = DqArray<'a>;
    type Number = DqNumber<'a>;

    fn as_object(&self) -> Option<DqObject<'a>> {
        match self.0 {
            Value::Map(map) => Some(DqObject(map)),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<DqArray<'a>> {
        match self.0 {
            Value::Array(items) => Some(DqArray(items)),
            _ => None,
        }
    }

    fn as_string(&self) -> Option<Cow<'a, str>> {
        match self.0 {
            Value::String(s) => Some(Cow::Borrowed(s)),
            _ => None,
        }
    }

    fn as_number(&self) -> Option<DqNumber<'a>> {
        match self.0 {
            Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::BigFloat(_) => {
                Some(DqNumber(self.0))
            }
            _ => None,
        }
    }

    fn as_boolean(&self) -> Option<bool> {
        match self.0 {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn is_null(&self) -> bool {
        matches!(self.0, Value::Null)
    }

    fn json_type(&self) -> JsonType {
        match self.0 {
            Value::Null => JsonType::Null,
            Value::Bool(_) => JsonType::Boolean,
            Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::BigFloat(_) => {
                JsonType::Number
            }
            Value::String(_) => JsonType::String,
            Value::Array(_) => JsonType::Array,
            Value::Map(_) => JsonType::Object,
        }
    }

    fn equals_value(&self, expected: &serde_json::Value) -> bool {
        equals(self.0, expected)
    }

    fn to_value(&self) -> Cow<'a, serde_json::Value> {
        Cow::Owned(instance_json(self.0))
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(NodeIdentity::new(
            std::ptr::from_ref::<Value>(self.0) as usize
        ))
    }
}

/// Iterator over a map node's members.
pub(crate) struct DqMembers<'a>(indexmap::map::Iter<'a, String, Value>);

impl<'a> Iterator for DqMembers<'a> {
    type Item = (&'a str, DqNode<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(k, v)| (k.as_str(), DqNode(v)))
    }
}

impl<'a> Object<'a, DqJson> for DqObject<'a> {
    type Node = DqNode<'a>;
    type MemberName = &'a str;
    type MembersIter = DqMembers<'a>;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn get(&self, key: &String) -> Option<DqNode<'a>> {
        self.0.get(key.as_str()).map(DqNode)
    }

    fn members(&self) -> DqMembers<'a> {
        DqMembers(self.0.iter())
    }
}

/// Iterator over an array node's items.
pub(crate) struct DqItems<'a>(std::slice::Iter<'a, Value>);

impl<'a> Iterator for DqItems<'a> {
    type Item = DqNode<'a>;

    fn next(&mut self) -> Option<DqNode<'a>> {
        self.0.next().map(DqNode)
    }
}

impl<'a> Array<'a, DqJson> for DqArray<'a> {
    type Node = DqNode<'a>;
    type ElementsIter = DqItems<'a>;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn elements(&self) -> DqItems<'a> {
        DqItems(self.0.iter())
    }

    /// `uniqueItems` by [`same_item`]: numbers by their exact value, two
    /// NaNs the same item.
    fn is_unique(&self) -> bool {
        let mut seen = std::collections::HashSet::with_capacity(self.0.len());
        self.0.iter().all(|item| seen.insert(Item(item)))
    }
}

/// An array item under `uniqueItems`: equal by [`same_item`], hashed so
/// that equal items hash alike (a number by the `f64` nearest it).
struct Item<'a>(&'a Value);

impl PartialEq for Item<'_> {
    fn eq(&self, other: &Self) -> bool {
        same_item(self.0, other.0)
    }
}

impl Eq for Item<'_> {}

impl std::hash::Hash for Item<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        hash_item(self.0, state);
    }
}

fn hash_item<H: std::hash::Hasher>(value: &Value, state: &mut H) {
    use std::hash::Hash;
    match value {
        Value::Null => state.write_u8(0),
        Value::Bool(b) => b.hash(state),
        Value::String(s) => s.hash(state),
        Value::Array(items) => {
            state.write_usize(items.len());
            for item in items {
                hash_item(item, state);
            }
        }
        // Order-independent: a sum of the members' own hashes.
        Value::Map(map) => {
            let sum = map.iter().fold(0_u64, |sum, (key, member)| {
                let mut hasher = std::hash::DefaultHasher::new();
                key.hash(&mut hasher);
                hash_item(member, &mut hasher);
                sum.wrapping_add(std::hash::Hasher::finish(&hasher))
            });
            state.write_u64(sum);
        }
        number => {
            let nearest = DqNumber(number).as_f64().unwrap_or(0.0);
            let bits = if nearest.is_nan() {
                f64::NAN.to_bits()
            } else if nearest == 0.0 {
                0
            } else {
                nearest.to_bits()
            };
            state.write_u64(bits);
        }
    }
}

/// Two items are the same under `uniqueItems`: JSON Schema equality —
/// numbers by their exact value, whatever their variant (`1` is `1.0`) —
/// and two NaNs are one item, as dq tells data apart
/// ([`dq_core::Value::same_data`]).
fn same_item(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_item(a, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_item(a, b)))
        }
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (a, b) => {
            a.same_number(b)
                || a.non_finite_float().is_some_and(f64::is_nan)
                    && b.non_finite_float().is_some_and(f64::is_nan)
        }
    }
}

/// `const`, in place of jsonschema's own: that one compares a number past
/// `i64` as the `f64` nearest it, so `18446744073709551617` met
/// `const: 18446744073709551616`. Same message, same paths.
struct Const(serde_json::Value);

impl<'i> jsonschema::Keyword<'i, DqJson> for Const {
    fn validate(&self, instance: DqNode<'i>) -> Result<(), jsonschema::ValidationError<'i>> {
        if equals(instance.0, &self.0) {
            Ok(())
        } else {
            Err(jsonschema::ValidationError::custom(format!(
                "{} was expected",
                self.0
            )))
        }
    }

    fn is_valid(&self, instance: DqNode<'i>) -> bool {
        equals(instance.0, &self.0)
    }
}

/// `enum`, in place of jsonschema's own, for the reason of [`Const`] and
/// so that a schema's `.inf` (a [`marker`]) matches an infinity. Same
/// message.
struct Enum(Vec<serde_json::Value>);

impl Enum {
    /// How many options a message lists, as jsonschema's does.
    const LISTED: usize = 3;

    fn holds(&self, instance: &Value) -> bool {
        self.0.iter().any(|option| equals(instance, option))
    }

    /// `x is not one of a, b or c`, or `… a, b or 5 other candidates`.
    fn explain(&self, instance: &Value) -> String {
        let mut out = format!("{} is not one of ", instance_json(instance));
        let total = self.0.len();
        let shown = if total <= Self::LISTED {
            total
        } else {
            Self::LISTED - 1
        };
        for (i, option) in self.0.iter().take(shown).enumerate() {
            if i > 0 {
                out.push_str(if total <= Self::LISTED && i == total - 1 {
                    " or "
                } else {
                    ", "
                });
            }
            out.push_str(&option.to_string());
        }
        if total > Self::LISTED {
            out.push_str(&format!(" or {} other candidates", total - shown));
        }
        out
    }
}

impl<'i> jsonschema::Keyword<'i, DqJson> for Enum {
    fn validate(&self, instance: DqNode<'i>) -> Result<(), jsonschema::ValidationError<'i>> {
        if self.holds(instance.0) {
            Ok(())
        } else {
            Err(jsonschema::ValidationError::custom(
                self.explain(instance.0),
            ))
        }
    }

    fn is_valid(&self, instance: DqNode<'i>) -> bool {
        self.holds(instance.0)
    }
}

/// A bound — `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`
/// — in place of jsonschema's own, which reads both numbers as `u64`,
/// `i64` or else the `f64` nearest them: `18446744073709551617` met
/// `maximum: 18446744073709551616`, and a NaN met every bound. Here both
/// compare by their exact value ([`Value::compare_number`]); a NaN, which
/// has no order, fails every bound. jsonschema's messages.
struct Bound {
    keyword: &'static str,
    /// The limit as the schema's source text spells it ([`spellings`]),
    /// else as serde_json does.
    limit: String,
    /// The limit's exact value.
    exact: Value,
}

impl Bound {
    const KEYWORDS: [&'static str; 4] =
        ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"];

    /// Whether `ordering` — the instance's against the limit — holds.
    fn admits(&self, ordering: Ordering) -> bool {
        match self.keyword {
            "minimum" => ordering != Ordering::Less,
            "maximum" => ordering != Ordering::Greater,
            "exclusiveMinimum" => ordering == Ordering::Greater,
            _ => ordering == Ordering::Less,
        }
    }

    /// `None` when `instance` meets the bound (a value that is not a
    /// number meets every one), else why it does not.
    fn check(&self, instance: &Value) -> Option<String> {
        let ordered = match instance {
            Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::BigFloat(_) => {
                instance.compare_number(&self.exact)
            }
            _ => return None,
        };
        if ordered.is_some_and(|ordering| self.admits(ordering)) {
            return None;
        }
        let instance = instance_json(instance);
        let limit = &self.limit;
        match ordered {
            Some(_) => Some(match self.keyword {
                "minimum" => format!("{instance} is less than the minimum of {limit}"),
                "maximum" => format!("{instance} is greater than the maximum of {limit}"),
                "exclusiveMinimum" => {
                    format!("{instance} is less than or equal to the minimum of {limit}")
                }
                _ => format!("{instance} is greater than or equal to the maximum of {limit}"),
            }),
            None => Some(match self.keyword {
                "minimum" => {
                    format!("{instance} has no order, so it is not at least the minimum of {limit}")
                }
                "maximum" => {
                    format!("{instance} has no order, so it is not at most the maximum of {limit}")
                }
                "exclusiveMinimum" => {
                    format!("{instance} has no order, so it is not above the minimum of {limit}")
                }
                _ => format!("{instance} has no order, so it is not below the maximum of {limit}"),
            }),
        }
    }
}

impl<'i> jsonschema::Keyword<'i, DqJson> for Bound {
    fn validate(&self, instance: DqNode<'i>) -> Result<(), jsonschema::ValidationError<'i>> {
        match self.check(instance.0) {
            None => Ok(()),
            Some(message) => Err(jsonschema::ValidationError::custom(message)),
        }
    }

    fn is_valid(&self, instance: DqNode<'i>) -> bool {
        self.check(instance.0).is_none()
    }
}

/// `multipleOf` in place of jsonschema's own, which computes in `f64`:
/// it found `.nan` a multiple of `0.5`, every finite number one of
/// `1e400` (`x / inf` is `0`, a whole number), and spelled a divisor past
/// 64 bits as the `f64` nearest it. Here `x` is a multiple of `d` when
/// `x / d` is an integer, by their exact values
/// ([`Value::is_multiple_of`]); a NaN and an infinity are multiples of
/// nothing. jsonschema's message, with the divisor as the schema spells it.
struct MultipleOf {
    /// The divisor as the schema spells it.
    spelling: String,
    /// The divisor's exact value.
    exact: Value,
}

impl MultipleOf {
    /// `None` when `instance` is a multiple of the divisor (a value that is
    /// not a number is one), else why it is not.
    fn check(&self, instance: &Value) -> Option<String> {
        match instance {
            Value::Int(_) | Value::BigInt(_) | Value::Float(_) | Value::BigFloat(_) => {}
            _ => return None,
        }
        if instance.is_multiple_of(&self.exact) == Some(true) {
            return None;
        }
        Some(format!(
            "{} is not a multiple of {}",
            instance_json(instance),
            self.spelling
        ))
    }
}

impl<'i> jsonschema::Keyword<'i, DqJson> for MultipleOf {
    fn validate(&self, instance: DqNode<'i>) -> Result<(), jsonschema::ValidationError<'i>> {
        match self.check(instance.0) {
            None => Ok(()),
            Some(message) => Err(jsonschema::ValidationError::custom(message)),
        }
    }

    fn is_valid(&self, instance: DqNode<'i>) -> bool {
        self.check(instance.0).is_none()
    }
}

/// The text dq puts in band, where the validator alone sees it: the
/// members it adds to the copy of a schema the validator compiles
/// ([`tiny_divisor`], [`spellings`]: [`compiled_json`]) and a non-finite
/// float's [`marker`], in that copy or in an instance as an error renders
/// it. The schema a rule holds has none of it ([`SchemaNotes`]).
///
/// Each begins with `\0dq` and a tag of 128 random bits drawn once per
/// process, so no text written before dq ran — a rule, a schema file, a
/// document — can spell one: a member named `"\u0000dq.spelling"` is the
/// rule's own, and a string `"\u0000dq.inf\u0000"` a string. Nothing
/// prints the tag: a message spells a marker `.inf`, and a printed schema
/// leaves dq's members out.
struct InBand {
    /// `\0dq` and the tag: what a [`marker`] begins with.
    prefix: String,
    /// The prefix of every member dq adds to a schema.
    member: String,
    /// [`tiny_divisor`].
    tiny_divisor: String,
    /// [`spellings`].
    spellings: String,
}

fn in_band() -> &'static InBand {
    static IN_BAND: std::sync::OnceLock<InBand> = std::sync::OnceLock::new();
    IN_BAND.get_or_init(|| {
        use std::hash::{BuildHasher as _, Hasher as _};
        // std seeds each `RandomState` from the OS's random source; the
        // hash of nothing under its keys is as random as they are.
        let word = || {
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        };
        let prefix = format!("\u{0}dq{:016x}{:016x}", word(), word());
        let member = format!("{prefix}.");
        InBand {
            tiny_divisor: format!("{member}multipleOf"),
            spellings: format!("{member}spelling"),
            prefix,
            member,
        }
    })
}

/// The member that holds a `multipleOf` divisor's literal next to the
/// stand-in [`compiled_json`] puts in its place.
fn tiny_divisor() -> &'static str {
    &in_band().tiny_divisor
}

/// The member of a schema, in the copy [`compiled_json`] makes, that maps a
/// limit keyword (`maximum`, `multipleOf`, …) to the limit's source text,
/// where serde_json cannot keep it: it reads `1e2` as `1e+2` and `1E-1` as
/// `1e-1`. dq's bounds and `multipleOf` quote the limit from here
/// ([`note_limit_spellings`]).
fn spellings() -> &'static str {
    &in_band().spellings
}

/// The keywords whose limit a message quotes.
const LIMIT_KEYWORDS: [&str; 5] = [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
];

/// What dq knows of a schema that the schema's JSON cannot hold, kept
/// beside the JSON rather than in it, each note by its place in the
/// schema: an RFC 6901 pointer from the schema's root (`/properties/x/maximum`).
///
/// - a limit's source text where serde_json spells the limit otherwise —
///   it reads `1e2` as `1e+2` — so a message quotes the limit as the rule
///   or the schema file writes it ([`note_limit_spellings`]);
/// - where a YAML schema holds a NaN or an infinity, which JSON has no
///   number for: the JSON holds its spelling there as a string (`".inf"`),
///   and the note tells it from the string `".inf"` ([`schema_json`]).
///
/// Since no note is a member of the schema, a member a rule names like one
/// of dq's (`"\u0000dq.spelling"`) is the rule's own: it changes no limit
/// and no message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaNotes {
    /// The source text of a limit, by the limit's place.
    spellings: BTreeMap<String, String>,
    /// `.nan`, `.inf` or `-.inf`, by the place of the value that is one.
    non_finite: BTreeMap<String, &'static str>,
}

impl SchemaNotes {
    /// Whether there is no note.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spellings.is_empty() && self.non_finite.is_empty()
    }

    /// The source text of the limit at `place` (`/maximum` → `1e2`), when
    /// serde_json spells it otherwise.
    #[must_use]
    pub fn spelling_at(&self, place: &str) -> Option<&str> {
        self.spellings.get(place).map(String::as_str)
    }

    /// `.nan`, `.inf` or `-.inf`, when the value at `place` is that number
    /// rather than the string its JSON holds.
    #[must_use]
    pub fn non_finite_at(&self, place: &str) -> Option<&'static str> {
        self.non_finite.get(place).copied()
    }
}

/// The place of `keyword`'s value in the schema at `at`, as
/// [`SchemaNotes`] keys it.
fn limit_place(at: &dq_core::Pointer, keyword: &str) -> String {
    at.with_segment(dq_core::Segment::Key(keyword.to_owned()))
        .as_canonical()
}

/// The source text noted for `keyword` in `schema`, a schema of the copy
/// [`compiled_json`] makes.
fn noted_spelling(
    schema: &serde_json::Map<String, serde_json::Value>,
    keyword: &str,
) -> Option<String> {
    schema
        .get(spellings())?
        .get(keyword)?
        .as_str()
        .map(str::to_owned)
}

/// Note in `notes`, for each schema in `schema`, the source text of every
/// limit ([`LIMIT_KEYWORDS`]) that serde_json spells otherwise, so a
/// message quotes the limit as the rule or the schema file writes it
/// (`1e2`, not `1e+2`).
///
/// `at` is where `schema` sits in the source and `literal_at` gives the
/// source text of the value at a place in the source. The text is noted
/// only when it is a JSON number of the limit's value: other text (`+5`,
/// `.5`) keeps the number's own spelling. Only a schema's limits are
/// noted ([`walk_schemas`]): in data (`const`, `enum`) `maximum` is no
/// keyword.
pub(crate) fn note_limit_spellings(
    schema: &serde_json::Value,
    at: &dq_core::Pointer,
    literal_at: &dyn Fn(&dq_core::Pointer) -> Option<String>,
    notes: &mut SchemaNotes,
) {
    walk_schemas(
        schema,
        &mut dq_core::Pointer::default(),
        &mut |object, inner| {
            for keyword in LIMIT_KEYWORDS {
                let Some(limit) = object.get(keyword) else {
                    continue;
                };
                let mut source = at.clone();
                for segment in inner.segments() {
                    source.push_segment(segment.clone());
                }
                source.push_segment(dq_core::Segment::Key(keyword.to_owned()));
                if let Some(text) = source_spelling(limit, &literal_at(&source)) {
                    notes.spellings.insert(limit_place(inner, keyword), text);
                }
            }
        },
    );
}

/// `text`, the source text of the limit `value`, when it is a JSON number
/// of the same value that serde_json spells otherwise.
fn source_spelling(value: &serde_json::Value, text: &Option<String>) -> Option<String> {
    let serde_json::Value::Number(number) = value else {
        return None;
    };
    let text = text.as_deref()?;
    let literal = text.parse::<serde_json::Number>().ok()?;
    let same = Value::from_serde_json(&serde_json::Value::Number(literal))
        .same_number(&Value::from_serde_json(value));
    (same && text != number.to_string()).then(|| text.to_owned())
}

/// The copy of `schema` the validator compiles: `schema` with what
/// `notes` holds put in band, under the tag no rule can spell ([`InBand`]).
///
/// - each NaN or infinity becomes its [`marker`], which dq's `const` and
///   `enum` match against that number only, and which fails to compile
///   where a number must stand (`maximum: .inf`);
/// - each schema with a noted limit spelling gets a [`spellings`] member,
///   which dq's bounds and `multipleOf` quote the limit from;
/// - every positive `multipleOf` divisor that the `f64` nearest it reads
///   as `0` (`1e-400`) is replaced by `1`, its literal kept next to it
///   under [`tiny_divisor`]. The meta-schema's `exclusiveMinimum: 0` reads
///   the divisor as an `f64`, so such a divisor did not compile (`1e-400 is
///   less than or equal to the minimum of 0`); and the meta-schema check
///   cannot be turned off. dq's `multipleOf` reads the literal back.
///
/// Borrowed when there is nothing to put in.
pub(crate) fn compiled_json<'s>(
    schema: &'s serde_json::Value,
    notes: &SchemaNotes,
) -> Cow<'s, serde_json::Value> {
    if notes.is_empty() && !has_tiny_divisor(schema) {
        return Cow::Borrowed(schema);
    }
    let mut owned = schema.clone();
    for (place, spelling) in &notes.non_finite {
        if let Some(value) = owned.pointer_mut(place) {
            *value = serde_json::Value::String(marker(spelling));
        }
    }
    walk_schemas_mut(
        &mut owned,
        &mut dq_core::Pointer::default(),
        &mut |object, at| {
            let mut noted = serde_json::Map::new();
            for keyword in LIMIT_KEYWORDS {
                if !matches!(object.get(keyword), Some(serde_json::Value::Number(_))) {
                    continue;
                }
                if let Some(text) = notes.spelling_at(&limit_place(at, keyword)) {
                    noted.insert(
                        keyword.to_owned(),
                        serde_json::Value::String(text.to_owned()),
                    );
                }
            }
            if !noted.is_empty() {
                object.insert(spellings().to_owned(), serde_json::Value::Object(noted));
            }
            stand_in_tiny_divisor(object);
        },
    );
    Cow::Owned(owned)
}

/// What a keyword's value is in a JSON Schema 2020-12 schema.
#[derive(Clone, Copy)]
enum Place {
    /// A schema: `not`, `items`, `additionalProperties`, …
    Schema,
    /// An array of schemas: `allOf`, `prefixItems`, …
    Schemas,
    /// A map of names to schemas: `properties`, `$defs`, …
    SchemaMap,
    /// Anything else: a limit, data (`const`, `enum`, `default`), an
    /// annotation, or a keyword 2020-12 does not have.
    Data,
}

/// The keywords whose value is a schema.
const SCHEMA_KEYWORDS: [&str; 11] = [
    "items",
    "contains",
    "additionalProperties",
    "propertyNames",
    "if",
    "then",
    "else",
    "not",
    "unevaluatedItems",
    "unevaluatedProperties",
    "contentSchema",
];

/// The keywords whose value is an array of schemas.
const SCHEMA_LISTS: [&str; 4] = ["prefixItems", "allOf", "anyOf", "oneOf"];

/// The keywords whose value maps names to schemas: a member named
/// `multipleOf` there is a schema.
const SCHEMA_MAPS: [&str; 5] = [
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

/// What the value of `keyword` is in a schema.
fn place_of(keyword: &str) -> Place {
    if SCHEMA_KEYWORDS.contains(&keyword) {
        Place::Schema
    } else if SCHEMA_LISTS.contains(&keyword) {
        Place::Schemas
    } else if SCHEMA_MAPS.contains(&keyword) {
        Place::SchemaMap
    } else {
        Place::Data
    }
}

/// Call `visit` on `node`, when it is a schema object, and on every schema
/// object in it, each with where it sits (`at` is where `node` does).
///
/// Only a value of the shape its keyword takes is walked — an object for
/// `not`, an array for `allOf` — so a member dq adds never lands in a value
/// the meta-schema refuses, whose error would print it.
fn walk_schemas_mut(
    node: &mut serde_json::Value,
    at: &mut dq_core::Pointer,
    visit: &mut dyn FnMut(&mut serde_json::Map<String, serde_json::Value>, &dq_core::Pointer),
) {
    let serde_json::Value::Object(schema) = node else {
        return;
    };
    visit(schema, at);
    for (keyword, value) in schema.iter_mut() {
        let place = place_of(keyword);
        if matches!(place, Place::Data) {
            continue;
        }
        at.push_segment(dq_core::Segment::Key(keyword.clone()));
        match (place, value) {
            (Place::Schema, value) => walk_schemas_mut(value, at, visit),
            (Place::Schemas, serde_json::Value::Array(items)) => {
                for (index, item) in items.iter_mut().enumerate() {
                    at.push_segment(dq_core::Segment::Index(index));
                    walk_schemas_mut(item, at, visit);
                    at.pop_segment();
                }
            }
            (Place::SchemaMap, serde_json::Value::Object(map)) => {
                for (name, item) in map.iter_mut() {
                    at.push_segment(dq_core::Segment::Key(name.clone()));
                    walk_schemas_mut(item, at, visit);
                    at.pop_segment();
                }
            }
            _ => {}
        }
        at.pop_segment();
    }
}

/// [`walk_schemas_mut`] without changing `node`.
fn walk_schemas(
    node: &serde_json::Value,
    at: &mut dq_core::Pointer,
    visit: &mut dyn FnMut(&serde_json::Map<String, serde_json::Value>, &dq_core::Pointer),
) {
    let serde_json::Value::Object(schema) = node else {
        return;
    };
    visit(schema, at);
    for (keyword, value) in schema {
        let place = place_of(keyword);
        if matches!(place, Place::Data) {
            continue;
        }
        at.push_segment(dq_core::Segment::Key(keyword.clone()));
        match (place, value) {
            (Place::Schema, value) => walk_schemas(value, at, visit),
            (Place::Schemas, serde_json::Value::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    at.push_segment(dq_core::Segment::Index(index));
                    walk_schemas(item, at, visit);
                    at.pop_segment();
                }
            }
            (Place::SchemaMap, serde_json::Value::Object(map)) => {
                for (name, item) in map {
                    at.push_segment(dq_core::Segment::Key(name.clone()));
                    walk_schemas(item, at, visit);
                    at.pop_segment();
                }
            }
            _ => {}
        }
        at.pop_segment();
    }
}

/// Whether `node` or a schema in it ([`walk_schemas_mut`]) passes `test`.
fn any_schema(
    node: &serde_json::Value,
    test: &dyn Fn(&serde_json::Map<String, serde_json::Value>) -> bool,
) -> bool {
    let serde_json::Value::Object(schema) = node else {
        return false;
    };
    test(schema)
        || schema
            .iter()
            .any(|(keyword, value)| match (place_of(keyword), value) {
                (Place::Schema, value) => any_schema(value, test),
                (Place::Schemas, serde_json::Value::Array(items)) => {
                    items.iter().any(|item| any_schema(item, test))
                }
                (Place::SchemaMap, serde_json::Value::Object(map)) => {
                    map.values().any(|item| any_schema(item, test))
                }
                _ => false,
            })
}

/// Whether `divisor` is a positive number that its nearest `f64` reads as
/// zero or less.
fn is_tiny_divisor(divisor: &serde_json::Value) -> bool {
    let serde_json::Value::Number(number) = divisor else {
        return false;
    };
    number.as_f64().is_none_or(|f| f <= 0.0)
        && Value::from_serde_json(divisor).compare_number(&Value::Int(0)) == Some(Ordering::Greater)
}

/// Whether some schema in `node` has a tiny `multipleOf`.
fn has_tiny_divisor(node: &serde_json::Value) -> bool {
    any_schema(node, &|schema| {
        schema.get("multipleOf").is_some_and(is_tiny_divisor)
    })
}

/// Put `1` in place of `schema`'s `multipleOf` when it is a tiny divisor,
/// its literal next to it under [`tiny_divisor`] ([`compiled_json`]).
fn stand_in_tiny_divisor(schema: &mut serde_json::Map<String, serde_json::Value>) {
    let literal = match schema.get_mut("multipleOf") {
        Some(divisor) if is_tiny_divisor(divisor) => {
            let literal = divisor.to_string();
            *divisor = serde_json::Value::from(1);
            literal
        }
        _ => return,
    };
    schema.insert(
        tiny_divisor().to_owned(),
        serde_json::Value::String(literal),
    );
}

/// `schema` as a message prints it (`not`), in serde_json's compact JSON
/// but for what dq holds in it: a limit is spelled as its source text
/// ([`spellings`]), a tiny divisor as written ([`tiny_divisor`]), a NaN or
/// an infinity as `.nan` / `.inf` / `-.inf`, and no member dq adds is
/// printed.
fn render_schema(schema: &serde_json::Value) -> String {
    let mut out = String::new();
    render(schema, Place::Schema, &mut out);
    out
}

/// [`render_schema`] for `node`, which is a `place`.
fn render(node: &serde_json::Value, place: Place, out: &mut String) {
    match (place, node) {
        (Place::Schema, serde_json::Value::Object(schema)) => {
            out.push('{');
            let members = schema
                .iter()
                .filter(|(k, _)| !k.starts_with(&in_band().member));
            for (index, (keyword, value)) in members.enumerate() {
                if index > 0 {
                    out.push(',');
                }
                render_key(keyword, out);
                match value {
                    serde_json::Value::Number(number)
                        if LIMIT_KEYWORDS.contains(&keyword.as_str()) =>
                    {
                        let text = noted_spelling(schema, keyword)
                            .or_else(|| {
                                (keyword == "multipleOf")
                                    .then(|| {
                                        schema.get(tiny_divisor())?.as_str().map(str::to_owned)
                                    })
                                    .flatten()
                            })
                            .unwrap_or_else(|| number.to_string());
                        out.push_str(&text);
                    }
                    value => render(value, place_of(keyword), out),
                }
            }
            out.push('}');
        }
        (Place::Schemas, serde_json::Value::Array(items)) => {
            render_array(items, Place::Schema, out);
        }
        (Place::SchemaMap, serde_json::Value::Object(map)) => {
            render_object(map, Place::Schema, out);
        }
        (_, serde_json::Value::Array(items)) => render_array(items, Place::Data, out),
        (_, serde_json::Value::Object(map)) => render_object(map, Place::Data, out),
        (_, serde_json::Value::String(s)) => match unmarker(s) {
            Some(spelling) => out.push_str(spelling),
            None => out.push_str(&serde_json::Value::String(s.clone()).to_string()),
        },
        (_, scalar) => out.push_str(&scalar.to_string()),
    }
}

/// [`render`] for an array whose items are each a `place`.
fn render_array(items: &[serde_json::Value], place: Place, out: &mut String) {
    out.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        render(item, place, out);
    }
    out.push(']');
}

/// [`render`] for an object whose values are each a `place`.
fn render_object(map: &serde_json::Map<String, serde_json::Value>, place: Place, out: &mut String) {
    out.push('{');
    for (index, (key, value)) in map.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        render_key(key, out);
        render(value, place, out);
    }
    out.push('}');
}

/// `key` as JSON writes an object key, and the colon after it.
fn render_key(key: &str, out: &mut String) {
    out.push_str(&serde_json::Value::String(key.to_owned()).to_string());
    out.push(':');
}

/// The text of a validation error as dq reports it: a schema the message
/// prints (`not`) as [`render_schema`] does, and every non-finite float
/// [`restore`]d.
pub(crate) fn error_text(error: &jsonschema::ValidationError<'_>) -> String {
    let text = match error.kind() {
        jsonschema::error::ValidationErrorKind::Not { schema } => format!(
            "{} is not allowed for {}",
            render_schema(schema),
            error.instance()
        ),
        _ => error.to_string(),
    };
    restore(&text).into_owned()
}

/// The keywords [`options`] puts in place of jsonschema's own.
const OVERRIDDEN: [&str; 7] = [
    "const",
    "enum",
    "multipleOf",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
];

/// The evaluation paths — the keywords followed to an error, a `$ref`
/// included (`/properties/x/$ref/enum`) — of the errors that dq's own
/// keywords raise.
///
/// jsonschema gives a custom keyword's error its schema location as its
/// evaluation path (`/$defs/x/enum`: it holds that a custom keyword is
/// never reached through a `$ref`), so these come from its `evaluate`
/// output instead, which follows the `$ref`. Built on the first such
/// error only; every other error carries its own.
pub(crate) struct EvaluationPaths<'v> {
    validator: &'v jsonschema::Validator<DqJson>,
    instance: &'v Value,
    /// `(instance location, keyword, message)` to the evaluation paths of
    /// the errors with them, in evaluation order.
    paths: Option<HashMap<(String, String, String), VecDeque<String>>>,
}

impl<'v> EvaluationPaths<'v> {
    pub(crate) fn new(validator: &'v jsonschema::Validator<DqJson>, instance: &'v Value) -> Self {
        Self {
            validator,
            instance,
            paths: None,
        }
    }

    /// The evaluation path of `error`, an error of `instance` under
    /// `validator`. Call it once per error, in `iter_errors` order: two
    /// errors alike take the paths in turn.
    pub(crate) fn of(&mut self, error: &jsonschema::ValidationError<'_>) -> String {
        let jsonschema::error::ValidationErrorKind::Custom { keyword, message } = error.kind()
        else {
            return error.evaluation_path().as_str().to_owned();
        };
        if !OVERRIDDEN.contains(&keyword.as_str()) {
            return error.evaluation_path().as_str().to_owned();
        }
        let (validator, instance) = (self.validator, self.instance);
        let paths = self.paths.get_or_insert_with(|| {
            let evaluation = validator.evaluate(DqNode(instance));
            let mut paths = HashMap::new();
            collect_paths(evaluation.root(), &mut paths);
            paths
        });
        let key = (
            error.instance_path().as_str().to_owned(),
            keyword.clone(),
            message.clone(),
        );
        paths
            .get_mut(&key)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| error.evaluation_path().as_str().to_owned())
    }
}

/// File the errors of `unit` and of every unit below it, in order.
fn collect_paths(
    unit: jsonschema::OutputUnit<'_>,
    paths: &mut HashMap<(String, String, String), VecDeque<String>>,
) {
    for error in unit.errors() {
        let mut path = unit.evaluation_path().as_str().to_owned();
        if !path.ends_with(&format!("/{}", error.keyword())) {
            path.push('/');
            path.push_str(error.keyword());
        }
        paths
            .entry((
                unit.instance_location().as_str().to_owned(),
                error.keyword().to_owned(),
                error.message().to_owned(),
            ))
            .or_default()
            .push_back(path);
    }
    for child in unit.children() {
        collect_paths(child, paths);
    }
}

/// jsonschema's options for JSON Schema 2020-12 over [`DqJson`], without
/// dq's own keywords. `offline()` swaps the default retriever for one
/// that refuses every URI not already in the registry, so even if
/// `resolve-http` / `resolve-file` were ever unified on by another crate
/// in the graph, the validator cannot reach the network or the
/// filesystem.
fn builtin_options<'i>()
-> jsonschema::ValidationOptions<'i, std::sync::Arc<dyn jsonschema::Retrieve>, DqJson> {
    jsonschema::options_for::<DqJson>()
        .with_draft(jsonschema::Draft::Draft202012)
        .offline()
}

/// Options for an offline JSON Schema 2020-12 validator over [`DqJson`],
/// with `const`, `enum`, the bounds and `multipleOf` that read NaN,
/// infinities and big numbers as the module docs say.
pub(crate) fn options<'i>()
-> jsonschema::ValidationOptions<'i, std::sync::Arc<dyn jsonschema::Retrieve>, DqJson> {
    let mut options = builtin_options()
        .with_keyword("const", |_, expected, _| {
            Ok(Box::new(Const(expected.clone())))
        })
        .with_keyword("enum", |_, options, _| match options {
            serde_json::Value::Array(options) => Ok(Box::new(Enum(options.clone()))),
            other => Err(jsonschema::ValidationError::custom(format!(
                "{other} is not of type \"array\""
            ))),
        })
        .with_keyword("multipleOf", |schema, divisor, _| {
            // A tiny divisor's literal stands next to its stand-in.
            let literal = schema
                .get(tiny_divisor())
                .and_then(serde_json::Value::as_str)
                .and_then(|literal| serde_json::from_str::<serde_json::Value>(literal).ok());
            match literal.as_ref().unwrap_or(divisor) {
                divisor @ serde_json::Value::Number(_) => Ok(Box::new(MultipleOf {
                    spelling: noted_spelling(schema, "multipleOf")
                        .unwrap_or_else(|| divisor.to_string()),
                    exact: Value::from_serde_json(divisor),
                })),
                other => Err(jsonschema::ValidationError::custom(format!(
                    "{other} is not of type \"number\""
                ))),
            }
        });
    for keyword in Bound::KEYWORDS {
        options = options.with_keyword(keyword, move |schema, limit, _| match limit {
            serde_json::Value::Number(_) => Ok(Box::new(Bound {
                keyword,
                limit: noted_spelling(schema, keyword).unwrap_or_else(|| limit.to_string()),
                exact: Value::from_serde_json(limit),
            })),
            other => Err(jsonschema::ValidationError::custom(format!(
                "{other} is not of type \"number\""
            ))),
        });
    }
    options
}

impl JsonNumber for DqNumber<'_> {
    fn as_u64(&self) -> Option<u64> {
        match self.0 {
            Value::Int(i) => u64::try_from(*i).ok(),
            Value::BigInt(s) => s.parse().ok(),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self.0 {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// A NaN and an infinity as themselves; a big number as the `f64`
    /// nearest it, an infinity past the `f64` range.
    fn as_f64(&self) -> Option<f64> {
        if let Some(f) = self.0.non_finite_float() {
            return Some(f);
        }
        match self.0 {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::BigInt(s) | Value::BigFloat(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// The literal, or `.nan` / `.inf` / `-.inf` — read only for the sign
    /// of a number no `f64` holds.
    fn as_str(&self) -> Cow<'_, str> {
        if let Some(spelling) = self.0.non_finite_spelling() {
            return Cow::Borrowed(spelling);
        }
        match self.0 {
            Value::BigInt(s) | Value::BigFloat(s) => Cow::Borrowed(s),
            other => Cow::Owned(other.to_serde_json().to_string()),
        }
    }

    /// The number as JSON spells it. jsonschema calls this only with its
    /// `arbitrary-precision` feature, which dq leaves off, and for
    /// `is_integer`, overridden below; no JSON number is a NaN or an
    /// infinity, so one reads as `0` here.
    fn to_number(&self) -> Cow<'_, serde_json::Number> {
        match self.0.to_serde_json() {
            serde_json::Value::Number(n) => Cow::Owned(n),
            _ => Cow::Owned(serde_json::Number::from(0)),
        }
    }

    fn is_integer(&self) -> bool {
        self.0.is_whole_number()
    }
}

/// `value` equals the schema value `expected` (`enum`, `const`): numbers
/// by their exact value, an infinity only the infinity of its sign, a
/// NaN nothing.
fn equals(value: &Value, expected: &serde_json::Value) -> bool {
    match (value, expected) {
        (Value::Null, serde_json::Value::Null) => true,
        (Value::Bool(a), serde_json::Value::Bool(b)) => a == b,
        (Value::String(a), serde_json::Value::String(b)) => a == b,
        (Value::Array(a), serde_json::Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equals(a, b))
        }
        (Value::Map(a), serde_json::Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| equals(a, b)))
        }
        (number, serde_json::Value::Number(n)) => number.same_number(&Value::from_serde_json(
            &serde_json::Value::Number(n.clone()),
        )),
        // A schema's own NaN or infinity, which only a YAML schema holds.
        (number, serde_json::Value::String(s)) => unmarker(s)
            .is_some_and(|spelling| number.same_number(&Value::BigFloat(spelling.to_owned()))),
        _ => false,
    }
}

/// The marker a NaN or an infinity is in [`instance_json`]: a string no
/// document holds ([`InBand`]), which JSON renders as
/// `"\u0000dq<tag>.nan\u0000"`.
fn marker(spelling: &str) -> String {
    format!("{}{spelling}\u{0}", in_band().prefix)
}

/// The spelling (`.nan`, `.inf`, `-.inf`) a [`marker`] stands for.
fn unmarker(s: &str) -> Option<&str> {
    s.strip_prefix(in_band().prefix.as_str())?
        .strip_suffix('\u{0}')
        .filter(|spelling| matches!(*spelling, ".nan" | ".inf" | "-.inf"))
}

/// A schema read as a [`Value`] (from YAML) as JSON: every number keeps
/// its exact literal (the workspace's `serde_json` is built with
/// `arbitrary_precision`), and a NaN or an infinity, which JSON has no
/// number for, stands as its spelling (`".inf"`), its place noted in
/// `notes`. The copy the validator compiles ([`compiled_json`]) holds a
/// [`marker`] there — a string the validator matches only against that
/// number (`const`, `enum`) and refuses where a number must stand
/// (`maximum: .inf`).
pub(crate) fn schema_json(schema: &Value, notes: &mut SchemaNotes) -> serde_json::Value {
    note_non_finite(schema, &mut dq_core::Pointer::default(), notes)
}

/// [`schema_json`] for `value`, which sits at `at` in the schema.
fn note_non_finite(
    value: &Value,
    at: &mut dq_core::Pointer,
    notes: &mut SchemaNotes,
) -> serde_json::Value {
    if let Some(spelling) = value.non_finite_spelling() {
        notes.non_finite.insert(at.as_canonical(), spelling);
        return serde_json::Value::String(spelling.to_owned());
    }
    match value {
        Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    at.push_segment(dq_core::Segment::Index(index));
                    let json = note_non_finite(item, at, notes);
                    at.pop_segment();
                    json
                })
                .collect(),
        ),
        Value::Map(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, child)| {
                    at.push_segment(dq_core::Segment::Key(key.clone()));
                    let json = note_non_finite(child, at, notes);
                    at.pop_segment();
                    (key.clone(), json)
                })
                .collect(),
        ),
        scalar => scalar.to_serde_json(),
    }
}

/// `value` as the JSON an error reports, a NaN or an infinity as a
/// [`marker`] string that [`restore`] turns back into `.nan` / `.inf` /
/// `-.inf`; a big number keeps its exact literal.
pub(crate) fn instance_json(value: &Value) -> serde_json::Value {
    if let Some(spelling) = value.non_finite_spelling() {
        return serde_json::Value::String(marker(spelling));
    }
    match value {
        Value::Array(items) => serde_json::Value::Array(items.iter().map(instance_json).collect()),
        Value::Map(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, child)| (k.clone(), instance_json(child)))
                .collect(),
        ),
        scalar => scalar.to_serde_json(),
    }
}

/// `message` with every [`marker`] it renders spelled `.nan` / `.inf` /
/// `-.inf`, as dq writes a non-finite float where JSON has no number.
pub(crate) fn restore(message: &str) -> Cow<'_, str> {
    if !message.contains("\\u0000dq") {
        return Cow::Borrowed(message);
    }
    let mut out = message.to_owned();
    for spelling in [".nan", "-.inf", ".inf"] {
        let rendered = serde_json::Value::String(marker(spelling)).to_string();
        out = out.replace(&rendered, spelling);
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validator(schema: serde_json::Value) -> jsonschema::Validator<DqJson> {
        options().build(&schema).expect("schema compiles")
    }

    fn big(literal: &str) -> Value {
        Value::BigFloat(literal.to_owned())
    }

    #[test]
    fn non_finite_floats_are_numbers() {
        let number = validator(serde_json::json!({"type": "number"}));
        let string = validator(serde_json::json!({"type": "string"}));
        let integer = validator(serde_json::json!({"type": "integer"}));
        for value in [
            Value::Float(f64::NAN),
            big(".nan"),
            big(".inf"),
            big("-.inf"),
        ] {
            assert!(number.is_valid(DqNode(&value)), "{value:?}");
            assert!(!string.is_valid(DqNode(&value)), "{value:?}");
            assert!(!integer.is_valid(DqNode(&value)), "{value:?}");
        }
    }

    #[test]
    fn bounds_order_infinities_and_refuse_nan() {
        let maximum = validator(serde_json::json!({"maximum": 10}));
        let minimum = validator(serde_json::json!({"minimum": 0}));
        assert!(!maximum.is_valid(DqNode(&big(".inf"))));
        assert!(minimum.is_valid(DqNode(&big(".inf"))));
        assert!(maximum.is_valid(DqNode(&big("-.inf"))));
        assert!(!minimum.is_valid(DqNode(&big("-.inf"))));
        assert!(!maximum.is_valid(DqNode(&big(".nan"))));
        assert!(!minimum.is_valid(DqNode(&big(".nan"))));
        assert!(!maximum.is_valid(DqNode(&big("1e400"))));
        assert!(minimum.is_valid(DqNode(&big("1e400"))));
    }

    #[test]
    fn multiple_of_divides_exact_values_and_refuses_nan_and_infinities() {
        for multiple in [serde_json::json!(2), serde_json::json!(0.5)] {
            let v = validator(serde_json::json!({"multipleOf": multiple}));
            for value in [big(".nan"), big(".inf"), big("-.inf")] {
                assert!(!v.is_valid(DqNode(&value)), "{multiple} {value:?}");
            }
            for value in [
                Value::BigInt("18446744073709551616".into()),
                Value::Int(4),
                big("1e400"),
            ] {
                assert!(v.is_valid(DqNode(&value)), "{multiple} {value:?}");
            }
            assert!(!v.is_valid(DqNode(&Value::Float(0.3))), "{multiple}");
        }
        let v = validator(serde_json::json!({"multipleOf": 0.5}));
        let message = v
            .iter_errors(DqNode(&big(".nan")))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(restore(&message), ".nan is not a multiple of 0.5");
        let maximum = validator(serde_json::json!({"maximum": 10}));
        let message = maximum
            .iter_errors(DqNode(&big(".nan")))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(
            restore(&message),
            ".nan has no order, so it is not at most the maximum of 10"
        );
        let message = maximum
            .iter_errors(DqNode(&big(".inf")))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(restore(&message), ".inf is greater than the maximum of 10");
        let message = v
            .iter_errors(DqNode(&Value::Float(0.3)))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(message, "0.3 is not a multiple of 0.5");
    }

    #[test]
    fn integer_type_reads_the_exact_value() {
        let integer = validator(serde_json::json!({"type": "integer"}));
        assert!(integer.is_valid(DqNode(&Value::BigInt("18446744073709551616".into()))));
        assert!(integer.is_valid(DqNode(&big("1e400"))));
        assert!(!integer.is_valid(DqNode(&big("1e-400"))));
        assert!(!integer.is_valid(DqNode(&Value::Float(1.5))));
    }

    #[test]
    fn enum_and_const_compare_numbers_by_value() {
        let one = validator(serde_json::json!({"enum": [1, "x"]}));
        assert!(one.is_valid(DqNode(&Value::Float(1.0))));
        assert!(!one.is_valid(DqNode(&big(".nan"))));
        let huge = validator(serde_json::from_str(r#"{"const": 18446744073709551616}"#).unwrap());
        assert!(huge.is_valid(DqNode(&Value::BigInt("18446744073709551616".into()))));
        assert!(!huge.is_valid(DqNode(&Value::BigInt("18446744073709551617".into()))));
        assert!(!huge.is_valid(DqNode(&big(".inf"))));
        let message = huge
            .iter_errors(DqNode(&Value::Int(1)))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(message, "18446744073709551616 was expected");
        let property = validator(serde_json::json!({"properties": {"a": {"const": 1}}}));
        let doc = |a: Value| Value::Map(IndexMap::from([("a".to_owned(), a)]));
        assert!(property.is_valid(DqNode(&doc(Value::Float(1.0)))));
        assert!(!property.is_valid(DqNode(&doc(Value::Int(2)))));
        let two = doc(Value::Int(2));
        let error = property.iter_errors(DqNode(&two)).next().expect("error");
        assert_eq!(error.instance_path().as_str(), "/a");
        assert_eq!(error.evaluation_path().as_str(), "/properties/a/const");
        let nested = validator(serde_json::json!({"const": {"a": [1, null]}}));
        assert!(nested.is_valid(DqNode(&Value::Map(IndexMap::from([(
            "a".to_owned(),
            Value::Array(vec![Value::Float(1.0), Value::Null]),
        )])))));
    }

    /// The copy the validator compiles of `schema`, read the way a YAML
    /// rule's schema is.
    fn yaml_compiled(schema: &str) -> serde_json::Value {
        let doc = dq_core::by_name("yaml")
            .expect("yaml format")
            .parse(schema.as_bytes())
            .expect("yaml schema");
        let mut notes = SchemaNotes::default();
        let json = schema_json(doc.value(), &mut notes);
        compiled_json(&json, &notes).into_owned()
    }

    /// A validator for `schema`, read the way a YAML rule's schema is.
    fn yaml_validator(schema: &str) -> jsonschema::Validator<DqJson> {
        options()
            .build(&yaml_compiled(schema))
            .expect("schema compiles")
    }

    /// A validator for the JSON `schema`, built as a rule's is.
    fn rule_validator(schema: &str) -> jsonschema::Validator<DqJson> {
        let schema: serde_json::Value = serde_json::from_str(schema).expect("JSON schema");
        options()
            .build(&compiled_json(&schema, &SchemaNotes::default()))
            .expect("schema compiles")
    }

    /// The limit spellings of the JSON schema `text`, noted as a schema
    /// file's are.
    fn file_notes(text: &str, schema: &serde_json::Value) -> SchemaNotes {
        let doc = dq_core::parse_json_with_spans(text.as_bytes()).expect("spans");
        let literal_at = |at: &dq_core::Pointer| {
            let span = doc.span_at(at)?;
            text.get(span.value_range.clone()).map(str::to_owned)
        };
        let mut notes = SchemaNotes::default();
        note_limit_spellings(
            schema,
            &dq_core::Pointer::default(),
            &literal_at,
            &mut notes,
        );
        notes
    }

    #[test]
    fn a_not_message_prints_a_tiny_divisor_as_written() {
        let not = rule_validator(r#"{"not": {"minimum": 0, "multipleOf": 1e-400}}"#);
        let tiny = big("2e-400");
        let error = not.iter_errors(DqNode(&tiny)).next().expect("error");
        assert_eq!(
            error_text(&error),
            r#"{"minimum":0,"multipleOf":1e-400} is not allowed for 2e-400"#
        );
        let plain = rule_validator(r#"{"not": {"multipleOf": 2}}"#);
        let error = plain
            .iter_errors(DqNode(&Value::Int(4)))
            .next()
            .expect("error");
        assert_eq!(
            error_text(&error),
            r#"{"multipleOf":2} is not allowed for 4"#
        );
    }

    #[test]
    fn multiple_of_reads_divisors_past_f64_exactly() {
        let huge = rule_validator(r#"{"multipleOf": 1e400}"#);
        assert!(huge.is_valid(DqNode(&Value::Int(0))));
        assert!(huge.is_valid(DqNode(&big("2e400"))));
        assert!(!huge.is_valid(DqNode(&Value::Int(4))));
        assert!(!huge.is_valid(DqNode(&Value::BigInt("18446744073709551616".into()))));
        let message = huge
            .iter_errors(DqNode(&Value::Int(4)))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(message, "4 is not a multiple of 1e+400");
        let wide = rule_validator(r#"{"multipleOf": 18446744073709551616}"#);
        let message = wide
            .iter_errors(DqNode(&Value::Int(4)))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(message, "4 is not a multiple of 18446744073709551616");
        let tiny = rule_validator(
            r#"{"properties": {"multipleOf": {"multipleOf": 1e-400}},
                "const": {"multipleOf": 1e-400}, "$defs": {"t": {"multipleOf": 3e-400}}}"#,
        );
        let doc = |x: Value| Value::Map([("multipleOf".to_owned(), x)].into_iter().collect());
        assert!(!tiny.is_valid(DqNode(&doc(big("1.5e-400")))));
        let tiny = rule_validator(r#"{"multipleOf": 1e-400}"#);
        assert!(tiny.is_valid(DqNode(&big("2e-400"))));
        assert!(tiny.is_valid(DqNode(&Value::Float(0.3))));
        assert!(!tiny.is_valid(DqNode(&big("1.5e-400"))));
        let message = tiny
            .iter_errors(DqNode(&big("1.5e-400")))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(message, "1.5e-400 is not a multiple of 1e-400");
        let schema = serde_json::json!({"const": {"multipleOf": 0}, "multipleOf": 2});
        assert!(matches!(
            compiled_json(&schema, &SchemaNotes::default()),
            Cow::Borrowed(_)
        ));
        let negative: serde_json::Value =
            serde_json::from_str(r#"{"multipleOf": -1e-400}"#).expect("JSON");
        assert!(
            options()
                .build(&compiled_json(&negative, &SchemaNotes::default()))
                .is_err()
        );
    }

    #[test]
    fn bounds_compare_exact_values() {
        let int = |s: &str| Value::BigInt(s.to_owned());
        let maximum = yaml_validator("maximum: 18446744073709551616\n");
        assert!(maximum.is_valid(DqNode(&int("18446744073709551616"))));
        assert!(!maximum.is_valid(DqNode(&int("18446744073709551617"))));
        let exclusive = yaml_validator("exclusiveMaximum: 0.10000000000000000001\n");
        assert!(exclusive.is_valid(DqNode(&Value::Float(0.1))));
        assert!(!exclusive.is_valid(DqNode(&big("0.10000000000000000001"))));
        let minimum = yaml_validator("minimum: 1e400\n");
        assert!(minimum.is_valid(DqNode(&big(".inf"))));
        assert!(minimum.is_valid(DqNode(&big("1e400"))));
        assert!(!minimum.is_valid(DqNode(&big("1e399"))));
        assert!(!minimum.is_valid(DqNode(&big(".nan"))));
        assert!(minimum.is_valid(DqNode(&Value::String("x".into()))));
        let exclusive_min = yaml_validator("exclusiveMinimum: -1\n");
        assert!(!exclusive_min.is_valid(DqNode(&Value::Float(-1.0))));
        let message = maximum
            .iter_errors(DqNode(&int("18446744073709551617")))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(
            message,
            "18446744073709551617 is greater than the maximum of 18446744073709551616"
        );
        let message = exclusive_min
            .iter_errors(DqNode(&Value::Int(-1)))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(message, "-1 is less than or equal to the minimum of -1");
    }

    #[test]
    fn a_yaml_schema_holds_exact_and_non_finite_numbers() {
        let huge = yaml_validator("const: 18446744073709551616\n");
        assert!(huge.is_valid(DqNode(&Value::BigInt("18446744073709551616".into()))));
        assert!(!huge.is_valid(DqNode(&Value::BigInt("18446744073709551617".into()))));
        let inf = yaml_validator("enum: [.inf, 1e400]\n");
        assert!(inf.is_valid(DqNode(&Value::Float(f64::INFINITY))));
        assert!(inf.is_valid(DqNode(&big("1.0e400"))));
        assert!(!inf.is_valid(DqNode(&big("-.inf"))));
        let message = inf
            .iter_errors(DqNode(&big("-.inf")))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(restore(&message), "-.inf is not one of .inf or 1e+400");
        let nan = yaml_validator("const: .nan\n");
        assert!(!nan.is_valid(DqNode(&big(".nan"))));
        let many = yaml_validator("enum: [1, 2, 3, 4]\n");
        let message = many
            .iter_errors(DqNode(&Value::Int(5)))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(message, "5 is not one of 1, 2 or 2 other candidates");
        let error = options()
            .build(&yaml_compiled("maximum: .inf\n"))
            .expect_err("no infinite bound");
        assert_eq!(
            restore(&error.to_string()),
            r#".inf is not of type "number""#
        );
        // The string `.inf` is a string: only the note makes one a number.
        let string = yaml_validator("enum: ['.inf']\n");
        assert!(string.is_valid(DqNode(&Value::String(".inf".to_owned()))));
        assert!(!string.is_valid(DqNode(&big(".inf"))));
        // A note finds its place under a name RFC 6901 escapes.
        let escaped = yaml_validator("properties: {'a/b~c': {const: -.inf}}\n");
        let doc = |x: Value| Value::Map(IndexMap::from([("a/b~c".to_owned(), x)]));
        assert!(escaped.is_valid(DqNode(&doc(big("-.inf")))));
        assert!(!escaped.is_valid(DqNode(&doc(Value::String("-.inf".to_owned())))));
    }

    #[test]
    fn unique_items_compares_numbers_by_value() {
        let unique = validator(serde_json::json!({"uniqueItems": true}));
        let array = |items: Vec<Value>| Value::Array(items);
        assert!(!unique.is_valid(DqNode(&array(vec![Value::Int(1), Value::Float(1.0)]))));
        assert!(!unique.is_valid(DqNode(&array(vec![big(".nan"), Value::Float(f64::NAN)]))));
        assert!(unique.is_valid(DqNode(&array(vec![
            Value::BigInt("18446744073709551616".into()),
            Value::BigInt("18446744073709551617".into()),
        ]))));
        assert!(unique.is_valid(DqNode(&array(vec![big(".inf"), big("-.inf"), big(".nan")]))));
    }

    #[test]
    fn messages_spell_non_finite_floats_the_yaml_way() {
        let string = validator(serde_json::json!({"type": "string"}));
        let doc = Value::Map(IndexMap::from([("x".to_owned(), big(".nan"))]));
        let array = validator(serde_json::json!({"type": "array"}));
        let message = array
            .iter_errors(DqNode(&doc))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(restore(&message), r#"{"x":.nan} is not of type "array""#);
        let message = string
            .iter_errors(DqNode(&big("-.inf")))
            .next()
            .expect("error")
            .to_string();
        assert_eq!(restore(&message), r#"-.inf is not of type "string""#);
        assert_eq!(restore("plain"), "plain");
    }

    #[test]
    fn a_limit_keeps_the_text_serde_json_spells_otherwise() {
        let text = r#"{"maximum": 1e2, "minimum": 5, "multipleOf": 2.5E0,
            "const": {"maximum": 1e2}, "properties": {"maximum": {"minimum": 1E1}},
            "prefixItems": [{"exclusiveMinimum": 1E-1}, {"maximum": 1e-1}]}"#;
        let schema: serde_json::Value = serde_json::from_str(text).expect("schema");
        let notes = file_notes(text, &schema);
        assert_eq!(notes.spelling_at("/maximum"), Some("1e2"));
        assert_eq!(notes.spelling_at("/multipleOf"), Some("2.5E0"));
        assert_eq!(notes.spelling_at("/minimum"), None);
        // Data and a property named like a keyword get no note.
        assert_eq!(notes.spelling_at("/const/maximum"), None);
        assert_eq!(notes.spelling_at("/properties/maximum"), None);
        assert_eq!(
            notes.spelling_at("/properties/maximum/minimum"),
            Some("1E1")
        );
        assert_eq!(
            notes.spelling_at("/prefixItems/0/exclusiveMinimum"),
            Some("1E-1")
        );
        // Text serde_json spells the same way needs no note.
        assert_eq!(notes.spelling_at("/prefixItems/1/maximum"), None);

        let schema = compiled_json(&schema, &notes);
        let validator = options().build(&schema).expect("compiles");
        let message = validator
            .iter_errors(DqNode(&Value::Int(101)))
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            message,
            [
                "101 is greater than the maximum of 1e2",
                "101 is not a multiple of 2.5E0",
                r#"{"maximum":1e+2} was expected"#,
            ]
        );
        assert_eq!(
            render_schema(&schema),
            r#"{"maximum":1e2,"minimum":5,"multipleOf":2.5E0,"const":{"maximum":1e+2},"properties":{"maximum":{"minimum":1E1}},"prefixItems":[{"exclusiveMinimum":1E-1},{"maximum":1e-1}]}"#
        );
    }

    #[test]
    fn a_member_named_like_a_note_beside_a_noted_limit_is_the_users() {
        let text = r#"{"not": {"maximum": 1e2, "\u0000dq.spelling": {"maximum": "g"},
            "\u0000dq.multipleOf": "7", "multipleOf": 1}}"#;
        let schema: serde_json::Value = serde_json::from_str(text).expect("schema");
        let notes = file_notes(text, &schema);
        assert_eq!(notes.spelling_at("/not/maximum"), Some("1e2"));
        let compiled = compiled_json(&schema, &notes);
        assert_eq!(
            render_schema(&compiled["not"]),
            r#"{"maximum":1e2,"\u0000dq.spelling":{"maximum":"g"},"\u0000dq.multipleOf":"7","multipleOf":1}"#
        );
        let inner = options().build(&compiled["not"]).expect("compiles");
        let messages: Vec<String> = inner
            .iter_errors(DqNode(&Value::Int(101)))
            .map(|error| error.to_string())
            .collect();
        assert_eq!(messages, ["101 is greater than the maximum of 1e2"]);
        // The schema a rule holds is the rule's alone: nothing dq added.
        assert_eq!(
            schema,
            serde_json::from_str::<serde_json::Value>(text).unwrap()
        );
    }

    #[test]
    fn text_spelled_like_dqs_in_band_text_is_the_users() {
        let forged = validator(serde_json::json!({
            "multipleOf": 1,
            "\u{0}dq.multipleOf": "7",
            "maximum": 5,
            "\u{0}dq.spelling": {"maximum": "g"}
        }));
        assert!(forged.is_valid(DqNode(&Value::Int(3))));
        let messages: Vec<String> = forged
            .iter_errors(DqNode(&Value::Int(6)))
            .map(|error| error.to_string())
            .collect();
        assert_eq!(messages, ["6 is greater than the maximum of 5"]);
        let inf = serde_json::json!("\u{0}dq.inf\u{0}");
        assert!(!equals(&big(".inf"), &inf));
        assert!(equals(&Value::String("\u{0}dq.inf\u{0}".to_owned()), &inf));
        assert_ne!(marker(".inf"), "\u{0}dq.inf\u{0}");
        assert_eq!(
            restore(r#""\u0000dq.inf\u0000""#),
            r#""\u0000dq.inf\u0000""#
        );
    }

    #[test]
    fn a_printed_schema_shows_only_what_the_rule_wrote() {
        let mut schema = serde_json::json!({
            "not": {"multipleOf": 1, "enum": [marker(".inf"), "a"], "x-data": {"k": marker("-.inf")}},
            "allOf": [{"maximum": 3}],
            "$defs": {"d": {"minimum": 0}},
            "const": {"\u{0}dq.member": 1}
        });
        schema["not"][tiny_divisor()] = serde_json::json!("1e-400");
        schema["allOf"][0][spellings()] = serde_json::json!({"maximum": "3.0e0"});
        schema["$defs"]["d"][spellings()] = serde_json::json!({"minimum": "0E0"});
        assert_eq!(
            render_schema(&schema),
            "{\"not\":{\"multipleOf\":1e-400,\"enum\":[.inf,\"a\"],\"x-data\":{\"k\":-.inf}},\
             \"allOf\":[{\"maximum\":3.0e0}],\"$defs\":{\"d\":{\"minimum\":0E0}},\
             \"const\":{\"\\u0000dq.member\":1}}"
        );
    }

    #[test]
    fn a_note_goes_only_where_the_keyword_takes_a_schema() {
        // `not` holding an array, `allOf` an object, `type` an object: the
        // meta-schema refuses each, and its error prints the value.
        let text = r#"{"not": [{"maximum": 1e2}], "allOf": {"a": {"maximum": 1e2}},
            "type": {"maximum": 1e2}}"#;
        let schema: serde_json::Value = serde_json::from_str(text).expect("schema");
        let notes = file_notes(text, &schema);
        assert!(notes.is_empty());
        assert!(matches!(compiled_json(&schema, &notes), Cow::Borrowed(_)));
    }

    #[test]
    fn text_of_another_value_or_no_json_number_is_not_noted() {
        let schema = serde_json::json!({"maximum": 100, "minimum": 5});
        let literal_at = |at: &dq_core::Pointer| match at.as_canonical().as_str() {
            "/maximum" => Some("1e3".to_owned()),
            _ => Some("+5".to_owned()),
        };
        let mut notes = SchemaNotes::default();
        note_limit_spellings(
            &schema,
            &dq_core::Pointer::default(),
            &literal_at,
            &mut notes,
        );
        assert!(notes.is_empty());
    }
}
