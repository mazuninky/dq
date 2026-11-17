//! Property tests for the [`dq_core::Pointer`] mutation API.
//!
//! Pins the observable equivalence between the functional builder
//! ([`Pointer::with_segment`]) and the in-place mutation API
//! ([`Pointer::push_segment`] / [`Pointer::pop_segment`]). The recursive
//! walks in `transform::diff` and `transform::merge_into` rely on this
//! equivalence — if push/pop ever diverges from `with_segment` (e.g. due to
//! a future storage change), the property fails and forces a deliberate
//! decision rather than a silent semantic break.
//!
//! Test plan tracked in
//! `openspec/changes/perf-pointer-recursive-walks/design.md` §5.1.

use dq_core::pointer::{Pointer, Segment};
use proptest::prelude::*;

/// Strategy for arbitrary single segments. Mixes `Key` and `Index` to
/// exercise both enum variants, and includes RFC 6901 escape-bait
/// characters (`~`, `/`) in keys so the property catches any future
/// canonicalisation bug in the mutation path.
fn any_segment() -> impl Strategy<Value = Segment> {
    prop_oneof![
        // Plain printable ASCII keys.
        "[a-zA-Z][a-zA-Z0-9_-]{0,8}".prop_map(Segment::Key),
        // Keys that exercise the RFC 6901 escape rules (`~` → `~0`,
        // `/` → `~1`). Equality is on the unescaped segments, not the
        // canonical render, but we still want these in the corpus so any
        // future storage that lossily normalises them gets caught.
        prop::sample::select(vec![
            "a/b".to_string(),
            "a~b".to_string(),
            "kubernetes.io/name".to_string(),
            "~slash~".to_string(),
        ])
        .prop_map(Segment::Key),
        // Array indices, bounded — large indices are equivalent in
        // structural terms, no need to fuzz beyond a few orders of
        // magnitude.
        (0usize..10_000).prop_map(Segment::Index),
    ]
}

/// Strategy for arbitrary pointers built from a vector of segments.
fn any_pointer() -> impl Strategy<Value = Pointer> {
    prop::collection::vec(any_segment(), 0..16).prop_map(Pointer::new)
}

proptest! {
    /// For any sequence of segments, the pointer built by chaining
    /// `with_segment` calls equals the pointer built by `push_segment`
    /// from a default root. This pins the central invariant the
    /// `transform::diff` / `transform::merge_into` refactor relies on —
    /// the recursive walks went from `with_segment`-on-`&self` to
    /// `push_segment`-on-`&mut self`, and the wire output must stay
    /// byte-identical.
    #[test]
    fn push_pop_matches_with_segment(segs in prop::collection::vec(any_segment(), 0..32)) {
        let mut chained = Pointer::default();
        for s in &segs {
            chained = chained.with_segment(s.clone());
        }

        let mut mutated = Pointer::default();
        for s in &segs {
            mutated.push_segment(s.clone());
        }

        prop_assert_eq!(chained.clone(), mutated.clone());
        // Canonical render must also match — this catches a hypothetical
        // bug where `Pointer::eq` and `Pointer::as_canonical` could
        // disagree (e.g. ordering invariants masked by `PartialEq`).
        prop_assert_eq!(chained.as_canonical(), mutated.as_canonical());
    }

    /// `push_segment` followed immediately by `pop_segment` is the
    /// identity. The recursive walks rely on this to balance their
    /// push/pop at the top and bottom of each loop iteration — if a
    /// future change ever sneaks state into `pop_segment`, this property
    /// fails loudly.
    #[test]
    fn push_then_pop_is_identity(start in any_pointer(), seg in any_segment()) {
        let original = start.clone();
        let mut p = start;
        p.push_segment(seg.clone());
        let popped = p.pop_segment();
        prop_assert_eq!(popped, Some(seg));
        prop_assert_eq!(p, original);
    }

    /// Balanced push/pop over an arbitrary sequence of segments returns
    /// the pointer to its starting state. Models the diff/merge inner
    /// loop where the pointer must be invariant across siblings — push
    /// at top, recurse, pop at bottom.
    #[test]
    fn balanced_push_pop_returns_to_start(
        start in any_pointer(),
        sibling_segs in prop::collection::vec(any_segment(), 0..8),
    ) {
        let original = start.clone();
        let mut p = start;
        for s in &sibling_segs {
            p.push_segment(s.clone());
            // Simulate an emit step in the recursive walk: an op clones
            // the fully-extended pointer for its own owned copy.
            let _emit = p.clone();
            p.pop_segment();
        }
        prop_assert_eq!(p, original);
    }
}
