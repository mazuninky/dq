//! JSON Pointer (RFC 6901) — parser, canonical renderer, and `Document` navigation.

use crate::Result;
use crate::document::Value;
use crate::error::{Error, PathErrorKind};

/// One segment of a parsed JSON Pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Object key.
    Key(String),
    /// Array index.
    Index(usize),
}

/// Parsed JSON Pointer.
///
/// Empty pointer (`""`) addresses the root of the document; any other valid
/// pointer starts with `/` and is composed of slash-separated segments where
/// `~0` un-escapes to `~` and `~1` to `/`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pointer(Vec<Segment>);

impl Pointer {
    /// Build a pointer from already-parsed segments.
    #[must_use]
    pub fn new(segments: Vec<Segment>) -> Self {
        Self(segments)
    }

    /// Returns the pointer's segments.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    /// Returns true when the pointer addresses the root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns true when the pointer's last segment is the RFC 6902 array-append
    /// marker `-` (a `Segment::Key("-")`).
    ///
    /// RFC 6902 §4.1 reserves the `-` token in the path of an `add` op as a
    /// shorthand for "the position past the end of the array". JSON Pointer
    /// (RFC 6901) does not coerce this token to an index, so it surfaces as a
    /// `Segment::Key("-")` here. Callers that want to treat `-` as an
    /// append-index pre-check this helper before `resolve` / `set_at`.
    #[must_use]
    pub fn is_array_append(&self) -> bool {
        matches!(self.0.last(), Some(Segment::Key(k)) if k == "-")
    }

    /// Build a new pointer that extends `self` by one segment.
    ///
    /// Used by the `transform::merge` recursion to walk children without
    /// mutating the parent pointer. The original pointer is left untouched
    /// and a new owning `Pointer` is returned.
    ///
    /// This call clones the internal `Vec<Segment>`, so each invocation is
    /// O(current depth). Prefer [`Pointer::push_segment`] /
    /// [`Pointer::pop_segment`] when walking a tree recursively — those keep
    /// the cost linear in the walk's depth rather than quadratic.
    #[must_use]
    pub fn with_segment(&self, seg: Segment) -> Self {
        let mut segs = self.0.clone();
        segs.push(seg);
        Self(segs)
    }

    /// Append a segment in place. O(1) amortized.
    ///
    /// Prefer this in recursive tree walks (e.g. `transform::diff`,
    /// `transform::merge_into`) that need to extend the pointer for descent
    /// and shrink it afterwards — pair with [`Pointer::pop_segment`]. For the
    /// one-shot "build a new owned pointer" use case (where the parent
    /// pointer must stay untouched and a fresh owning value is needed),
    /// see [`Pointer::with_segment`].
    pub fn push_segment(&mut self, seg: Segment) {
        self.0.push(seg);
    }

    /// Remove and return the last segment in place. O(1).
    ///
    /// Returns `None` when called on a root pointer. Pairs with
    /// [`Pointer::push_segment`] to unwind one level of recursion.
    pub fn pop_segment(&mut self) -> Option<Segment> {
        self.0.pop()
    }

    /// Parse an RFC 6901 pointer string.
    ///
    /// # Errors
    ///
    /// Returns `Error::Path` with `kind = TypeMismatch` for malformed input —
    /// e.g. a pointer that does not start with `/` or that contains an
    /// unescaped `~`.
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Ok(Self::default());
        }
        if !s.starts_with('/') {
            return Err(Error::Path {
                pointer: s.to_owned(),
                matched_prefix: String::new(),
                kind: PathErrorKind::TypeMismatch {
                    expected: "leading '/'",
                    found: "other",
                },
                did_you_mean: Vec::new(),
            });
        }
        // Skip the leading '/' before splitting; `split('/')` after a leading
        // '/' would yield a spurious empty first segment.
        let body = &s[1..];
        let mut segments = Vec::new();
        for raw in body.split('/') {
            if raw.is_empty() {
                // `dq` rejects empty segments (e.g. `//`) — they confuse users
                // far more than they help, even though strict RFC 6901 allows
                // them. The CLI surfaces this as a parse-time path error.
                return Err(Error::Path {
                    pointer: s.to_owned(),
                    matched_prefix: String::new(),
                    kind: PathErrorKind::TypeMismatch {
                        expected: "non-empty segment",
                        found: "empty segment",
                    },
                    did_you_mean: Vec::new(),
                });
            }
            let unescaped = unescape_segment(raw).map_err(|message| Error::Path {
                pointer: s.to_owned(),
                matched_prefix: String::new(),
                kind: PathErrorKind::TypeMismatch {
                    expected: "valid escape",
                    found: "invalid escape",
                },
                did_you_mean: vec![message],
            })?;
            segments.push(Segment::Key(unescaped));
        }
        Ok(Self(segments))
    }

    /// Render the pointer in canonical RFC 6901 form, re-escaping `~` and `/`.
    #[must_use]
    pub fn as_canonical(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for seg in &self.0 {
            out.push('/');
            match seg {
                Segment::Key(k) => out.push_str(&escape_segment(k)),
                Segment::Index(i) => {
                    use std::fmt::Write as _;
                    let _ = write!(out, "{i}");
                }
            }
        }
        out
    }

    /// Walk the pointer through `value`, returning the addressed node.
    ///
    /// # Errors
    ///
    /// Returns `Error::Path` with the longest matching prefix, the kind of
    /// failure, and (when applicable) up to three close-key suggestions.
    pub fn resolve<'a>(&self, value: &'a Value) -> Result<&'a Value> {
        let mut current = value;
        let mut matched: Vec<Segment> = Vec::new();
        for seg in &self.0 {
            match (current, seg) {
                (Value::Map(map), Segment::Key(k)) => {
                    if let Some(next) = map.get(k) {
                        current = next;
                        matched.push(Segment::Key(k.clone()));
                    } else {
                        let candidates: Vec<&str> = map.keys().map(String::as_str).collect();
                        return Err(Error::Path {
                            pointer: self.as_canonical(),
                            matched_prefix: Pointer(matched).as_canonical(),
                            kind: PathErrorKind::MissingKey,
                            did_you_mean: did_you_mean(k, &candidates),
                        });
                    }
                }
                (Value::Array(items), Segment::Key(k)) => {
                    // Try numeric coercion for the convenience of users who
                    // wrote `/0` and got it parsed as `Segment::Key("0")`.
                    if let Ok(idx) = k.parse::<usize>() {
                        if let Some(next) = items.get(idx) {
                            current = next;
                            matched.push(Segment::Index(idx));
                            continue;
                        }
                        return Err(Error::Path {
                            pointer: self.as_canonical(),
                            matched_prefix: Pointer(matched).as_canonical(),
                            kind: PathErrorKind::OutOfBounds,
                            did_you_mean: Vec::new(),
                        });
                    }
                    return Err(Error::Path {
                        pointer: self.as_canonical(),
                        matched_prefix: Pointer(matched).as_canonical(),
                        kind: PathErrorKind::TypeMismatch {
                            expected: "array index",
                            found: "non-numeric key",
                        },
                        did_you_mean: Vec::new(),
                    });
                }
                (Value::Array(items), Segment::Index(i)) => {
                    if let Some(next) = items.get(*i) {
                        current = next;
                        matched.push(Segment::Index(*i));
                    } else {
                        return Err(Error::Path {
                            pointer: self.as_canonical(),
                            matched_prefix: Pointer(matched).as_canonical(),
                            kind: PathErrorKind::OutOfBounds,
                            did_you_mean: Vec::new(),
                        });
                    }
                }
                (other, _) => {
                    return Err(Error::Path {
                        pointer: self.as_canonical(),
                        matched_prefix: Pointer(matched).as_canonical(),
                        kind: PathErrorKind::TypeMismatch {
                            expected: "object or array",
                            found: other.type_name(),
                        },
                        did_you_mean: Vec::new(),
                    });
                }
            }
        }
        Ok(current)
    }
}

fn unescape_segment(seg: &str) -> std::result::Result<String, String> {
    let mut out = String::with_capacity(seg.len());
    let mut chars = seg.chars();
    while let Some(c) = chars.next() {
        if c != '~' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => out.push('~'),
            Some('1') => out.push('/'),
            Some(other) => return Err(format!("invalid escape '~{other}'")),
            None => return Err("dangling '~' at end of segment".to_owned()),
        }
    }
    Ok(out)
}

fn escape_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for c in seg.chars() {
        match c {
            // Order matters: `~` must be escaped first, otherwise we'd
            // double-escape the `~` we just emitted for `/`.
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            other => out.push(other),
        }
    }
    out
}

/// Suggest up to three keys from `candidates` whose Levenshtein distance to
/// `missing` is at most 2. Sorted by distance ascending, then lexicographically.
#[must_use]
pub fn did_you_mean(missing: &str, candidates: &[&str]) -> Vec<String> {
    const MAX_DISTANCE: usize = 2;
    const MAX_SUGGESTIONS: usize = 3;
    let mut scored: Vec<(usize, &str)> = candidates
        .iter()
        .filter_map(|c| {
            let d = levenshtein(missing, c);
            (d <= MAX_DISTANCE).then_some((d, *c))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, s)| s.to_owned())
        .collect()
}

/// Compute the Levenshtein edit distance between `a` and `b`.
fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    // Two-row DP table.
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr: Vec<usize> = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use pretty_assertions::assert_eq;

    // ---- Pointer::parse — Task 3.1, ≥ 12 cases (existing + new) ----

    #[test]
    fn parse_root_is_empty() {
        let p = Pointer::parse("").unwrap();
        assert!(p.is_root(), "empty string must address root");
        assert_eq!(p.segments(), &[] as &[Segment]);
    }

    #[test]
    fn parse_single_key() {
        // `/foo` → exactly one segment.
        let p = Pointer::parse("/foo").unwrap();
        assert_eq!(p.segments(), &[Segment::Key("foo".into())]);
    }

    #[test]
    fn parse_nested_keys() {
        let p = Pointer::parse("/foo/bar").unwrap();
        assert_eq!(
            p.segments(),
            &[Segment::Key("foo".into()), Segment::Key("bar".into())]
        );
    }

    #[test]
    fn parse_numeric_segments_kept_as_keys() {
        // RFC 6901 leaves the segment a string at parse time; numeric coercion
        // happens in `resolve` when the container is an array. Verify both
        // segments come back as `Key`, not `Index`.
        let p = Pointer::parse("/0/1").unwrap();
        assert_eq!(
            p.segments(),
            &[Segment::Key("0".into()), Segment::Key("1".into())]
        );
    }

    #[test]
    fn parse_unescapes_tilde_zero_to_tilde() {
        // `~0` un-escapes to the literal `~` character.
        let p = Pointer::parse("/a~0b").unwrap();
        assert_eq!(p.segments(), &[Segment::Key("a~b".into())]);
    }

    #[test]
    fn parse_unescapes_tilde_one_to_slash() {
        // `~1` un-escapes to the literal `/` character.
        let p = Pointer::parse("/a~1b").unwrap();
        assert_eq!(p.segments(), &[Segment::Key("a/b".into())]);
    }

    #[test]
    fn parse_unescapes_tilde_and_slash() {
        let p = Pointer::parse("/a~0b/c~1d").unwrap();
        assert_eq!(
            p.segments(),
            &[Segment::Key("a~b".into()), Segment::Key("c/d".into())]
        );
    }

    #[test]
    fn parse_rejects_dangling_tilde() {
        // Bare `~` not followed by `0` or `1` is invalid.
        let err = Pointer::parse("/foo~").unwrap_err();
        assert_eq!(err.kind_name(), "path");
    }

    #[test]
    fn parse_rejects_unescaped_tilde() {
        // `~bar` (where `bar` does not start with 0 or 1) is invalid.
        let err = Pointer::parse("/foo~bar").unwrap_err();
        assert_eq!(err.kind_name(), "path");
    }

    #[test]
    fn parse_rejects_empty_segment() {
        // `dq` rejects empty segments (`//`) at parse-time. Strict RFC 6901
        // would allow this; the CLI treats it as a typo.
        let err = Pointer::parse("//").unwrap_err();
        assert_eq!(err.kind_name(), "path");
    }

    #[test]
    fn parse_rejects_no_leading_slash() {
        // Pointers other than the empty root MUST start with `/`.
        let err = Pointer::parse("foo").unwrap_err();
        assert_eq!(err.kind_name(), "path");
    }

    #[test]
    fn parse_keeps_leading_zero_index_as_key() {
        // An index segment with a leading zero (`/00`) is preserved as the
        // string `"00"` — the parser does not strip nor coerce. This matters
        // because some users distinguish `/0` from `/00` in object keys, and
        // resolve() does the numeric coercion when needed.
        let p = Pointer::parse("/00").unwrap();
        assert_eq!(p.segments(), &[Segment::Key("00".into())]);
    }

    #[test]
    fn parse_very_deep_pointer() {
        // ≥ 10 segments. Catches stack-overflow regressions if a recursive
        // implementation ever sneaks back in.
        let raw = "/a/b/c/d/e/f/g/h/i/j/k/l";
        let p = Pointer::parse(raw).unwrap();
        let segs = p.segments();
        assert_eq!(segs.len(), 12, "expected 12 segments, got {}", segs.len());
        assert_eq!(segs.first(), Some(&Segment::Key("a".into())));
        assert_eq!(segs.last(), Some(&Segment::Key("l".into())));
    }

    #[test]
    fn is_array_append_only_true_when_last_segment_is_dash() {
        // RFC 6902 §4.1: `-` at the tail of the path means "append".
        let p = Pointer::parse("/list/-").unwrap();
        assert!(p.is_array_append(), "tail `-` is the array-append marker");

        // A `-` mid-path is NOT the append marker — only the tail is special.
        let mid = Pointer::parse("/-/x").unwrap();
        assert!(
            !mid.is_array_append(),
            "tail `x` not `-` → should not flag as append",
        );

        // Empty pointer: no last segment, so cannot be the marker.
        assert!(!Pointer::default().is_array_append());

        // Numeric tail is NOT `-`.
        let numeric = Pointer::parse("/list/0").unwrap();
        assert!(!numeric.is_array_append());
    }

    #[test]
    fn with_segment_appends_without_mutating_original() {
        let base = Pointer::parse("/a").unwrap();
        let child = base.with_segment(Segment::Key("b".into()));
        assert_eq!(child.as_canonical(), "/a/b");
        // The original must be untouched — `with_segment` must clone.
        assert_eq!(base.as_canonical(), "/a");
    }

    #[test]
    fn push_then_pop_round_trips() {
        // Round-trip property: pushing a segment onto a default pointer and
        // then popping it must return the pointer to its original state. This
        // is the invariant that lets `transform::diff` push at the top of a
        // loop iteration and pop at the bottom without leaking state across
        // iterations.
        let mut p = Pointer::default();
        p.push_segment(Segment::Key("foo".into()));
        let popped = p.pop_segment();
        assert_eq!(popped, Some(Segment::Key("foo".into())));
        assert_eq!(p, Pointer::default());
    }

    #[test]
    fn pop_on_empty_returns_none() {
        // Defensive: `pop_segment` mirrors `Vec::pop`, which is a no-op on an
        // empty backing store rather than a panic. Recursive walks rely on
        // this — if an over-pop ever surfaces it should manifest as a
        // structural bug downstream, not as a midwalk crash.
        let mut p = Pointer::default();
        assert_eq!(p.pop_segment(), None);
        assert_eq!(p, Pointer::default());
    }

    #[test]
    fn push_segment_extends_segments() {
        // After `push_segment`, the segments slice ends with the pushed value
        // and grows by exactly one element. Pairs with the round-trip test
        // above as a positive-direction check.
        let mut p = Pointer::parse("/a").unwrap();
        p.push_segment(Segment::Key("b".into()));
        assert_eq!(p.segments().len(), 2);
        assert_eq!(p.segments().last(), Some(&Segment::Key("b".into())));
        // And the canonical render reflects the new tail segment.
        assert_eq!(p.as_canonical(), "/a/b");
    }

    #[test]
    fn push_pop_balanced_walk_returns_to_default() {
        // Models the diff/merge loop pattern: push at top, recurse, pop at
        // bottom. After a balanced walk through several siblings, the
        // pointer must be observably identical to where the walk started.
        let mut p = Pointer::default();
        for key in ["spec", "template", "spec"] {
            p.push_segment(Segment::Key(key.into()));
            // Simulate an `Add` / `Remove` emit while the path is extended:
            // the patch op clones the current pointer (its own owned copy)
            // and the walk pops it after, so the original pointer doesn't
            // retain the segment.
            let _emit = p.clone();
            p.pop_segment();
        }
        assert_eq!(
            p,
            Pointer::default(),
            "balanced push/pop must restore state"
        );
    }

    #[test]
    fn parse_unicode_keys() {
        // Cyrillic keys parse byte-correctly (`мета` is the Russian for "meta",
        // `имя` for "name"). RFC 6901 doesn't restrict char set.
        let p = Pointer::parse("/мета/имя").unwrap();
        assert_eq!(
            p.segments(),
            &[Segment::Key("мета".into()), Segment::Key("имя".into())]
        );
    }

    // ---- Pointer rendering / resolve ----

    #[test]
    fn canonical_form_re_escapes() {
        let p = Pointer::new(vec![Segment::Key("app.kubernetes.io/name".into())]);
        assert_eq!(p.as_canonical(), "/app.kubernetes.io~1name");
    }

    #[test]
    fn resolve_object_key() {
        let mut map = IndexMap::new();
        map.insert("port".into(), Value::Int(8080));
        let v = Value::Map(map);
        let p = Pointer::parse("/port").unwrap();
        assert_eq!(p.resolve(&v).unwrap(), &Value::Int(8080));
    }

    #[test]
    fn resolve_missing_key_suggests_close() {
        let mut map = IndexMap::new();
        map.insert("port".into(), Value::Int(8080));
        let v = Value::Map(map);
        let p = Pointer::parse("/prot").unwrap();
        let err = p.resolve(&v).unwrap_err();
        match err {
            Error::Path {
                did_you_mean, kind, ..
            } => {
                assert_eq!(kind, PathErrorKind::MissingKey);
                assert_eq!(did_you_mean, vec!["port".to_owned()]);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn resolve_array_index() {
        let v = Value::Array(vec![Value::Int(1), Value::Int(2)]);
        let p = Pointer::parse("/1").unwrap();
        assert_eq!(p.resolve(&v).unwrap(), &Value::Int(2));
    }

    #[test]
    fn resolve_out_of_bounds() {
        let v = Value::Array(vec![Value::Int(1)]);
        let p = Pointer::parse("/9").unwrap();
        let err = p.resolve(&v).unwrap_err();
        match err {
            Error::Path { kind, .. } => assert_eq!(kind, PathErrorKind::OutOfBounds),
            _ => panic!("wrong variant"),
        }
    }

    // ---- Levenshtein / did_you_mean — Task 3.2, ≥ 6 cases ----

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("port", "prot"), 2);
    }

    #[test]
    fn did_you_mean_orders_and_caps() {
        // Distances: porte=1; host=2 (p→h, r→s); porter=2; prot=2.
        // After sort by distance asc then lex, take 3: porte, host, porter.
        let suggestions = did_you_mean("port", &["host", "prot", "porte", "porter"]);
        assert_eq!(suggestions, vec!["porte", "host", "porter"]);
    }

    #[test]
    fn did_you_mean_returns_distance_ordered() {
        // Levenshtein distances vs `port`:
        // - `porte` = 1 (insert `e` at the tail)
        // - `host`  = 2 (substitute p→h, r→s; o and t match)
        // - `prot`  = 2 (substitute o→r and r→o — Levenshtein has no
        //   transposition op, so a swap costs 2 substitutions)
        // Sorted by distance asc, ties broken lexicographically:
        //   porte (d=1), host (d=2), prot (d=2).
        let suggestions = did_you_mean("port", &["host", "prot", "porte"]);
        assert_eq!(
            suggestions,
            vec!["porte".to_owned(), "host".to_owned(), "prot".to_owned()],
            "expected distance-1 candidate first, ties broken lexicographically"
        );
    }

    #[test]
    fn did_you_mean_filters_distant_candidates() {
        // `entirely_unrelated` is far enough away that it must NOT appear.
        let suggestions = did_you_mean("port", &["entirely_unrelated", "prot"]);
        assert_eq!(suggestions, vec!["prot"]);
    }

    #[test]
    fn did_you_mean_kubernetes_typo() {
        // The motivating real-world case: users typing `kubernates` when they
        // mean `kubernetes` (distance = 2: insertion of `e`, deletion of `a`).
        let suggestions = did_you_mean("kubernates", &["kubernetes"]);
        assert_eq!(
            suggestions,
            vec!["kubernetes".to_owned()],
            "k8s typo `kubernates` must suggest `kubernetes` (distance ≤ 2)"
        );
    }

    #[test]
    fn did_you_mean_no_candidates_returns_empty() {
        // Nothing within distance 2 — empty `Vec`, never an `Err`.
        let suggestions = did_you_mean("foo", &["zzzzzzzz", "qqqqqqqq"]);
        assert!(
            suggestions.is_empty(),
            "expected no suggestions, got: {suggestions:?}"
        );
        // And the empty-candidates list shape:
        let no_candidates = did_you_mean("foo", &[]);
        assert!(no_candidates.is_empty());
    }

    #[test]
    fn did_you_mean_beyond_distance_two_filtered() {
        // `port` vs `pXrtY` is distance 2 (substitute X, append Y) — KEPT.
        // `port` vs `aaaa` is distance 4 — DROPPED.
        // `port` vs `prtZZ` is distance 3 — DROPPED.
        let suggestions = did_you_mean("port", &["aaaa", "prtZZ", "pXrtY"]);
        assert_eq!(
            suggestions,
            vec!["pXrtY".to_owned()],
            "only candidates within distance 2 of `port` are returned"
        );
    }

    #[test]
    fn did_you_mean_caps_at_three() {
        // Five candidates within distance 2; result must contain exactly three.
        // All single-character substitutions of `port`:
        // `pXrt` (d=1), `pYrt` (d=1), `pZrt` (d=1), `aort` (d=1), `bort` (d=1).
        let suggestions = did_you_mean("port", &["pXrt", "pYrt", "pZrt", "aort", "bort"]);
        assert_eq!(
            suggestions.len(),
            3,
            "did_you_mean must cap result at 3, got: {suggestions:?}"
        );
        // Sorted by distance (all d=1), then lex: aort, bort, pXrt.
        assert_eq!(suggestions, vec!["aort", "bort", "pXrt"]);
    }

    #[test]
    fn did_you_mean_breaks_ties_lexicographically() {
        // Two candidates at the same distance: lexicographic order decides.
        // `pXrt`, `pYrt`, `pZrt` all distance 1 from `port`.
        let suggestions = did_you_mean("port", &["pZrt", "pYrt", "pXrt"]);
        assert_eq!(suggestions, vec!["pXrt", "pYrt", "pZrt"]);
    }
}
