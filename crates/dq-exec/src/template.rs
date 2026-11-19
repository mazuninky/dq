//! Mustache-style minimal template engine for `check.message`.
//!
//! This is the deliberately-tiny renderer described in design D4: every
//! `{{ … }}` marker is a path expression rooted at the violation value.
//! There are no conditionals, no loops, no helpers, no escape sequences.
//!
//! Supported syntax (whitespace inside `{{ }}` is trimmed):
//!
//! - `{{ . }}` — the entire violation value, rendered as compact JSON.
//! - `{{ .field }}` — object field lookup. String values render bare
//!   (without surrounding quotes); non-string values render as compact
//!   JSON.
//! - `{{ .a.b }}` — nested object lookup.
//! - `{{ .arr.0 }}` — array index by integer literal.
//!
//! A NaN or an infinity renders `.nan` / `.inf` / `-.inf`, bare, also
//! inside compact JSON (`{"x":.nan}`); a big number renders its exact
//! literal.
//!
//! Unknown paths render as the literal `<missing>` instead of erroring —
//! this is the documented contract so rules can reference optional fields
//! without crashing the run.

/// Render `template`, substituting every `{{ path }}` with the value at
/// `path` inside `value`.
///
/// The template scanner walks `template` left-to-right, copying literal
/// text to the output and resolving each `{{ … }}` marker against the
/// violation value. When a path resolves to a string, the string content
/// is interpolated bare (without the surrounding JSON quotes); for any
/// other type the compact JSON form is used. When a path doesn't resolve
/// — including when the path syntax is malformed — the literal token
/// `<missing>` is substituted.
///
/// Literal `{` and `}` characters outside `{{ … }}` markers are preserved
/// verbatim. An unterminated `{{` (no matching `}}`) is treated as
/// literal text and copied through unchanged.
#[must_use]
pub fn render(template: &str, value: &dq_core::Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut cursor = 0;
    let bytes = template.as_bytes();

    while cursor < bytes.len() {
        // Find the next `{{` starting at `cursor`.
        let Some(open_rel) = template[cursor..].find("{{") else {
            // No more markers — copy the rest verbatim.
            out.push_str(&template[cursor..]);
            break;
        };
        let open = cursor + open_rel;
        // Copy literal text up to the marker.
        out.push_str(&template[cursor..open]);

        // Look for a matching `}}` after `{{`.
        let body_start = open + 2;
        let Some(close_rel) = template[body_start..].find("}}") else {
            // Unterminated marker — treat the remainder as literal.
            out.push_str(&template[open..]);
            break;
        };
        let body_end = body_start + close_rel;
        let body = template[body_start..body_end].trim();

        match lookup(value, body) {
            Some(v) => out.push_str(&render_value(v)),
            None => out.push_str("<missing>"),
        }

        cursor = body_end + 2;
    }

    out
}

/// Walk `value` following the path expression `path` (as it appears
/// inside `{{ … }}` after whitespace trimming) and return the targeted
/// node, or `None` when any segment is missing or the path is malformed.
///
/// Path syntax:
///
/// - `.` (the empty path after the leading dot) returns `value` itself.
/// - `.field` returns `value[field]` for objects.
/// - `.0` returns `value[0]` for arrays.
/// - Multi-segment paths (`.a.b`, `.arr.0`) chain the same operations.
///
/// Non-conforming paths (`..`, `.`, `field` without leading dot, mixed
/// separators) return `None` rather than panicking.
fn lookup<'a>(value: &'a dq_core::Value, path: &str) -> Option<&'a dq_core::Value> {
    if !path.starts_with('.') {
        return None;
    }
    let trimmed = &path[1..];
    if trimmed.is_empty() {
        return Some(value);
    }
    let mut current = value;
    for segment in trimmed.split('.') {
        if segment.is_empty() {
            // Empty segment (e.g. `..` produces an empty middle segment)
            // is malformed.
            return None;
        }
        if !is_valid_segment(segment) {
            return None;
        }
        current = match current {
            dq_core::Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            dq_core::Value::Map(map) if segment.parse::<usize>().is_err() => map.get(segment)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Return `true` if `segment` matches the path-segment grammar:
/// either a non-negative integer literal, or an identifier that starts
/// with `[a-zA-Z_]` and continues with `[a-zA-Z0-9_]*`.
fn is_valid_segment(segment: &str) -> bool {
    if segment.bytes().all(|b| b.is_ascii_digit()) {
        return !segment.is_empty();
    }
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Render a value for interpolation: strings become their bare
/// content; everything else is rendered as compact JSON, a NaN or an
/// infinity as `.nan` / `.inf` / `-.inf`.
fn render_value(v: &dq_core::Value) -> String {
    match v {
        dq_core::Value::String(s) => s.clone(),
        other => {
            let mut out = String::new();
            write_compact(other, &mut out);
            out
        }
    }
}

/// Compact JSON of `v`, with a NaN or an infinity spelled bare
/// (`.nan`), as dq writes one where JSON has no number.
fn write_compact(v: &dq_core::Value, out: &mut String) {
    if let Some(spelling) = v.non_finite_spelling() {
        out.push_str(spelling);
        return;
    }
    match v {
        dq_core::Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(item, out);
            }
            out.push(']');
        }
        dq_core::Value::Map(map) => {
            out.push('{');
            for (i, (key, member)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(key.clone()).to_string());
                out.push(':');
                write_compact(member, out);
            }
            out.push('}');
        }
        scalar => out.push_str(&scalar.to_serde_json().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// [`super::render`] over the `dq_core::Value` of a JSON value.
    fn render(template: &str, value: &serde_json::Value) -> String {
        super::render(template, &dq_core::Value::from_serde_json(value))
    }

    #[test]
    fn non_finite_and_big_numbers_render_as_dq_writes_them() {
        let doc = dq_core::Value::Map(indexmap::IndexMap::from([
            ("x".to_owned(), dq_core::Value::Float(f64::NAN)),
            (
                "y".to_owned(),
                dq_core::Value::Array(vec![
                    dq_core::Value::BigFloat("-.Inf".into()),
                    dq_core::Value::BigInt("18446744073709551616".into()),
                ]),
            ),
        ]));
        assert_eq!(super::render("{{ .x }}", &doc), ".nan");
        assert_eq!(
            super::render("{{ .y.0 }} {{ .y.1 }}", &doc),
            "-.inf 18446744073709551616"
        );
        assert_eq!(
            super::render("{{ . }}", &doc),
            r#"{"x":.nan,"y":[-.inf,18446744073709551616]}"#
        );
    }

    #[test]
    fn no_markers_returns_template_unchanged() {
        assert_eq!(render("plain text", &json!({})), "plain text");
        assert_eq!(render("", &json!({})), "");
    }

    #[test]
    fn dot_renders_whole_value_as_compact_json() {
        let value = json!({"a": 1, "b": 2});
        assert_eq!(render("{{ . }}", &value), r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn dot_renders_string_bare() {
        // `{{ . }}` on a string value still renders bare (string render
        // path) — this matches the rule that string values are
        // interpolated without surrounding quotes.
        let value = json!("hello");
        assert_eq!(render("{{ . }}", &value), "hello");
    }

    #[test]
    fn field_lookup_renders_string_without_quotes() {
        let value = json!({"name": "web"});
        assert_eq!(
            render("Container '{{ .name }}' uses :latest tag", &value),
            "Container 'web' uses :latest tag"
        );
    }

    #[test]
    fn field_lookup_renders_non_string_as_json() {
        let value = json!({"count": 42, "items": [1, 2]});
        assert_eq!(render("count={{ .count }}", &value), "count=42");
        assert_eq!(render("items={{ .items }}", &value), "items=[1,2]");
    }

    #[test]
    fn nested_field_lookup() {
        let value = json!({"meta": {"name": "deploy"}});
        assert_eq!(render("name={{ .meta.name }}", &value), "name=deploy");
    }

    #[test]
    fn array_index_lookup_by_integer() {
        let value = json!({"arr": ["a", "b", "c"]});
        assert_eq!(render("first={{ .arr.0 }}", &value), "first=a");
        assert_eq!(render("second={{ .arr.1 }}", &value), "second=b");
    }

    #[test]
    fn unknown_field_renders_as_missing() {
        let value = json!({"a": 1});
        assert_eq!(render("{{ .nonexistent }}", &value), "<missing>");
    }

    #[test]
    fn whitespace_inside_braces_is_trimmed() {
        let value = json!({"name": "x"});
        assert_eq!(render("{{.name}}", &value), "x");
        assert_eq!(render("{{ .name }}", &value), "x");
        assert_eq!(render("{{    .name    }}", &value), "x");
    }

    #[test]
    fn multiple_markers_on_one_line() {
        let value = json!({"a": 1, "b": 2});
        assert_eq!(render("a={{ .a }}, b={{ .b }}", &value), "a=1, b=2");
    }

    #[test]
    fn literal_braces_outside_markers_are_preserved() {
        let value = json!({"a": 1});
        assert_eq!(render("{not a marker}", &value), "{not a marker}");
        assert_eq!(render("{{ .a }} and {literal}", &value), "1 and {literal}");
    }

    #[test]
    fn invalid_path_renders_as_missing() {
        let value = json!({"a": 1});
        // Double-dot in middle is malformed.
        assert_eq!(render("{{ .. }}", &value), "<missing>");
        // Path without a leading dot is malformed.
        assert_eq!(render("{{ name }}", &value), "<missing>");
        // Trailing dot leaves an empty segment — malformed.
        assert_eq!(render("{{ .a. }}", &value), "<missing>");
        // Segment with unsupported character — malformed.
        assert_eq!(render("{{ .a-b }}", &value), "<missing>");
    }

    #[test]
    fn unterminated_marker_is_copied_literally() {
        let value = json!({});
        assert_eq!(render("prefix {{ .a", &value), "prefix {{ .a");
    }

    #[test]
    fn unicode_string_renders_correctly() {
        let value = json!({"name": "héllo-世界"});
        assert_eq!(render("hi {{ .name }}", &value), "hi héllo-世界");
    }

    #[test]
    fn boolean_and_null_render_as_json() {
        let value = json!({"flag": true, "missing": null});
        assert_eq!(render("flag={{ .flag }}", &value), "flag=true");
        assert_eq!(render("m={{ .missing }}", &value), "m=null");
    }

    #[test]
    fn array_out_of_bounds_renders_missing() {
        let value = json!({"arr": ["x"]});
        assert_eq!(render("{{ .arr.9 }}", &value), "<missing>");
    }

    #[test]
    fn lookup_helper_returns_none_for_invalid_paths() {
        let value = dq_core::Value::from_serde_json(&json!({"a": 1}));
        assert!(lookup(&value, "no-leading-dot").is_none());
        assert!(lookup(&value, "..").is_none());
        assert!(lookup(&value, ".").is_some()); // empty path = whole value
        assert!(lookup(&value, ".missing").is_none());
    }

    #[test]
    fn render_value_helper_strings_render_bare() {
        let value = |json| dq_core::Value::from_serde_json(&json);
        assert_eq!(render_value(&value(json!("hello"))), "hello");
        assert_eq!(render_value(&value(json!(42))), "42");
        assert_eq!(render_value(&value(json!([1, 2]))), "[1,2]");
    }
}
