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
//!   `exclusiveMaximum`) compare an infinity as the infinity of its sign
//!   (`.inf` fails every maximum and passes every minimum) and a NaN as
//!   unordered: it fails every bound, as every IEEE comparison with a NaN
//!   is false. `multipleOf` fails on both, and on a number past the
//!   `f64` range (`1e400`), which jsonschema has no exact arithmetic for;
//! - `enum` and `const` compare numbers by their exact value
//!   ([`dq_core::Value::same_number`]: `1` is `1.0`, a 40-digit integer is
//!   only itself). A NaN or an infinity equals no value a schema can hold
//!   (a schema is JSON, which has neither), so it fails every `enum` and
//!   `const`; under `uniqueItems` two NaNs are the same item;
//! - a big number past `f64` (`1e400`) is past every finite bound, of its
//!   own sign; one inside the range compares as the `f64` nearest it, as
//!   the limit itself does (jsonschema reads a limit as `u64` / `i64` /
//!   `f64`).
//!
//! The validator renders an error's instance as JSON, which has no NaN;
//! [`instance_json`] spells one as a marker string and [`restore`] puts
//! `.nan` / `.inf` / `-.inf` back into the message.

use std::borrow::Cow;

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

/// A numeric keyword around jsonschema's own, which reads a number as an
/// `f64` and so answers wrongly for some: a NaN met every bound and came
/// out a multiple of any fraction (`.nan` of `0.5`). The numbers
/// [`Guarded::refuses`] fail with [`Guarded::explain`]'s message; every
/// other value gets jsonschema's answer and message.
struct Guarded {
    /// jsonschema's validator for the keyword alone.
    builtin: jsonschema::Validator<DqJson>,
    /// The keyword, as the schema spells it.
    keyword: &'static str,
    /// The keyword's value in the schema.
    limit: serde_json::Value,
    /// Whether a number — as the `f64` nearest it — fails outright.
    refuses: Refuses,
}

/// Whether a number, as the `f64` nearest it, fails a guarded keyword.
type Refuses = fn(f64) -> bool;

impl Guarded {
    /// The numeric keywords dq guards, each with the numbers it refuses:
    /// the bounds a NaN (unordered), `multipleOf` a NaN, an infinity and a
    /// number past the `f64` range (no exact arithmetic for them).
    const KEYWORDS: [(&'static str, Refuses); 5] = [
        ("minimum", f64::is_nan),
        ("maximum", f64::is_nan),
        ("exclusiveMinimum", f64::is_nan),
        ("exclusiveMaximum", f64::is_nan),
        ("multipleOf", |f| !f.is_finite()),
    ];

    fn refused(&self, instance: DqNode<'_>) -> bool {
        instance
            .as_number()
            .is_some_and(|number| number.as_f64().is_none_or(self.refuses))
    }

    /// Why a refused number fails, worded as jsonschema words the keyword.
    fn explain(&self, instance: DqNode<'_>) -> String {
        let instance = instance_json(instance.0);
        let limit = &self.limit;
        match self.keyword {
            "multipleOf" => format!("{instance} is not a multiple of {limit}"),
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
        }
    }
}

impl<'i> jsonschema::Keyword<'i, DqJson> for Guarded {
    fn validate(&self, instance: DqNode<'i>) -> Result<(), jsonschema::ValidationError<'i>> {
        if self.refused(instance) {
            return Err(jsonschema::ValidationError::custom(self.explain(instance)));
        }
        match self.builtin.iter_errors(instance).next() {
            None => Ok(()),
            Some(error) => Err(jsonschema::ValidationError::custom(error.to_string())),
        }
    }

    fn is_valid(&self, instance: DqNode<'i>) -> bool {
        !self.refused(instance) && self.builtin.is_valid(instance)
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
/// with `const`, the bounds and `multipleOf` that read NaN, infinities and
/// big numbers as the module docs say.
pub(crate) fn options<'i>()
-> jsonschema::ValidationOptions<'i, std::sync::Arc<dyn jsonschema::Retrieve>, DqJson> {
    let mut options = builtin_options().with_keyword("const", |_, expected, _| {
        Ok(Box::new(Const(expected.clone())))
    });
    for (keyword, refuses) in Guarded::KEYWORDS {
        options = options.with_keyword(keyword, move |_, limit, _| {
            let builtin = builtin_options()
                .build(&serde_json::json!({ keyword: limit }))
                .map_err(|error| jsonschema::ValidationError::custom(error.to_string()))?;
            Ok(Box::new(Guarded {
                builtin,
                keyword,
                limit: limit.clone(),
                refuses,
            }))
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
/// by their exact value, a NaN or an infinity never.
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
        _ => false,
    }
}

/// The marker a NaN or an infinity is in [`instance_json`]: a string no
/// document holds, which JSON renders as `"\u0000dq.nan\u0000"`.
fn marker(spelling: &str) -> String {
    format!("\u{0}dq{spelling}\u{0}")
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
    fn multiple_of_fails_where_no_f64_holds_the_value() {
        for multiple in [serde_json::json!(2), serde_json::json!(0.5)] {
            let v = validator(serde_json::json!({"multipleOf": multiple}));
            for value in [big(".nan"), big(".inf"), big("-.inf"), big("1e400")] {
                assert!(!v.is_valid(DqNode(&value)), "{multiple} {value:?}");
            }
            for value in [Value::BigInt("18446744073709551616".into()), Value::Int(4)] {
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
