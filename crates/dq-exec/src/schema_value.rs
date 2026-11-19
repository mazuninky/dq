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
//!   ([`stand_in_tiny_divisors`]); a NaN and an infinity are multiples of
//!   nothing;
//! - `enum` and `const` compare numbers by their exact value
//!   ([`dq_core::Value::same_number`]: `1` is `1.0`, a 40-digit integer is
//!   only itself); an infinity equals only the infinity of its sign (which
//!   only a YAML schema can hold), a NaN nothing; under `uniqueItems` two
//!   NaNs are the same item.
//!
//! The schema itself is JSON whose numbers keep their literal (the
//! workspace builds `serde_json` with `arbitrary_precision`); a YAML
//! schema's NaN or infinity is a [`marker`] string ([`schema_json`]), so
//! where a number must stand (`maximum: .inf`) the schema does not compile.
//!
//! The validator renders an error's instance as JSON, which has no NaN;
//! [`instance_json`] spells one as a marker string and [`restore`] puts
//! `.nan` / `.inf` / `-.inf` back into the message.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

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
    /// The limit as the schema spells it.
    limit: serde_json::Value,
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
        let instance = instance_json(instance);
        let limit = &self.limit;
        match ordered {
            Some(ordering) if self.admits(ordering) => None,
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

/// The member that holds a `multipleOf` divisor's literal next to the
/// stand-in [`stand_in_tiny_divisors`] puts in its place.
const TINY_DIVISOR: &str = "\u{0}dq.multipleOf";

/// `schema` with every positive `multipleOf` divisor that the `f64`
/// nearest it reads as `0` (`1e-400`) replaced by `1`, its literal kept
/// next to it under [`TINY_DIVISOR`].
///
/// The meta-schema's `exclusiveMinimum: 0` reads the divisor as an `f64`,
/// so such a divisor did not compile (`1e-400 is less than or equal to the
/// minimum of 0`); and the meta-schema check cannot be turned off. dq's
/// `multipleOf` reads the literal back. Borrowed when there is none.
pub(crate) fn stand_in_tiny_divisors(schema: &serde_json::Value) -> Cow<'_, serde_json::Value> {
    if !has_tiny_divisor(schema) {
        return Cow::Borrowed(schema);
    }
    let mut owned = schema.clone();
    replace_tiny_divisors(&mut owned);
    Cow::Owned(owned)
}

/// Keywords whose value is data, not a schema: no `multipleOf` inside is
/// one.
const DATA_KEYWORDS: [&str; 4] = ["const", "enum", "default", "examples"];

/// Keywords whose value maps names to schemas: a member named
/// `multipleOf` there is a schema.
const SCHEMA_MAPS: [&str; 5] = [
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

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
    match node {
        serde_json::Value::Object(schema) => {
            schema
                .iter()
                .any(|(keyword, value)| match keyword.as_str() {
                    "multipleOf" => is_tiny_divisor(value),
                    k if DATA_KEYWORDS.contains(&k) => false,
                    k if SCHEMA_MAPS.contains(&k) => value
                        .as_object()
                        .is_some_and(|map| map.values().any(has_tiny_divisor)),
                    _ => has_tiny_divisor(value),
                })
        }
        serde_json::Value::Array(items) => items.iter().any(has_tiny_divisor),
        _ => false,
    }
}

/// [`stand_in_tiny_divisors`] in place.
fn replace_tiny_divisors(node: &mut serde_json::Value) {
    match node {
        serde_json::Value::Object(schema) => {
            let mut literal = None;
            for (keyword, value) in schema.iter_mut() {
                match keyword.as_str() {
                    "multipleOf" if is_tiny_divisor(value) => {
                        literal = Some(value.to_string());
                        *value = serde_json::Value::from(1);
                    }
                    k if DATA_KEYWORDS.contains(&k) => {}
                    k if SCHEMA_MAPS.contains(&k) => {
                        if let Some(map) = value.as_object_mut() {
                            map.values_mut().for_each(replace_tiny_divisors);
                        }
                    }
                    _ => replace_tiny_divisors(value),
                }
            }
            if let Some(literal) = literal {
                schema.insert(TINY_DIVISOR.to_owned(), serde_json::Value::String(literal));
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(replace_tiny_divisors),
        _ => {}
    }
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
                .get(TINY_DIVISOR)
                .and_then(serde_json::Value::as_str)
                .and_then(|literal| serde_json::from_str::<serde_json::Value>(literal).ok());
            match literal.as_ref().unwrap_or(divisor) {
                divisor @ serde_json::Value::Number(_) => Ok(Box::new(MultipleOf {
                    spelling: divisor.to_string(),
                    exact: Value::from_serde_json(divisor),
                })),
                other => Err(jsonschema::ValidationError::custom(format!(
                    "{other} is not of type \"number\""
                ))),
            }
        });
    for keyword in Bound::KEYWORDS {
        options = options.with_keyword(keyword, move |_, limit, _| match limit {
            serde_json::Value::Number(_) => Ok(Box::new(Bound {
                keyword,
                limit: limit.clone(),
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
/// document holds, which JSON renders as `"\u0000dq.nan\u0000"`.
fn marker(spelling: &str) -> String {
    format!("\u{0}dq{spelling}\u{0}")
}

/// The spelling (`.nan`, `.inf`, `-.inf`) a [`marker`] stands for.
fn unmarker(s: &str) -> Option<&str> {
    s.strip_prefix("\u{0}dq")?
        .strip_suffix('\u{0}')
        .filter(|spelling| matches!(*spelling, ".nan" | ".inf" | "-.inf"))
}

/// A schema read as a [`Value`] (from YAML) as the JSON the validator
/// compiles: every number keeps its exact literal (the workspace's
/// `serde_json` is built with `arbitrary_precision`), and a NaN or an
/// infinity, which JSON has no number for, becomes its [`marker`] — a
/// string the validator matches only against that number (`const`,
/// `enum`) and refuses where a number must stand (`maximum: .inf`).
pub(crate) fn schema_json(schema: &Value) -> serde_json::Value {
    instance_json(schema)
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

    /// A validator for `schema`, read the way a YAML rule's schema is.
    fn yaml_validator(schema: &str) -> jsonschema::Validator<DqJson> {
        let doc = dq_core::by_name("yaml")
            .expect("yaml format")
            .parse(schema.as_bytes())
            .expect("yaml schema");
        options()
            .build(&schema_json(doc.value()))
            .expect("schema compiles")
    }

    /// A validator for the JSON `schema`, built as a rule's is.
    fn rule_validator(schema: &str) -> jsonschema::Validator<DqJson> {
        let schema: serde_json::Value = serde_json::from_str(schema).expect("JSON schema");
        options()
            .build(&stand_in_tiny_divisors(&schema))
            .expect("schema compiles")
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
        assert!(matches!(stand_in_tiny_divisors(&schema), Cow::Borrowed(_)));
        let negative: serde_json::Value =
            serde_json::from_str(r#"{"multipleOf": -1e-400}"#).expect("JSON");
        assert!(options().build(&stand_in_tiny_divisors(&negative)).is_err());
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
            .build(&schema_json(
                dq_core::by_name("yaml")
                    .expect("yaml")
                    .parse(b"maximum: .inf\n")
                    .expect("yaml")
                    .value(),
            ))
            .expect_err("no infinite bound");
        assert_eq!(
            restore(&error.to_string()),
            r#".inf is not of type "number""#
        );
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
}
