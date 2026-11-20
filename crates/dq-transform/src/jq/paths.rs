//! jq's path-creating assignment semantics on top of jaq.
//!
//! jq creates what a path assignment walks through and does not find:
//! `{} | .a.b = 1` is `{"a":{"b":1}}`, `null | .[2] = 1` is
//! `[null,null,1]`, and `del` of a missing path does nothing. jaq 3 runs
//! the assignment operators natively on its own value type and fails on
//! the same programs ("cannot use null as iterable", "index 2 out of
//! bounds"); its `setpath`, `delpaths` and `del` are defined through those
//! operators and fail with them.
//!
//! The jaq crates stay unpatched. Instead:
//!
//! - [`rewrite`] parses the program and, when it holds an assignment
//!   operator (`=`, `|=`, `+=` and the other arithmetic ones, `//=`),
//!   prints it back with every `L op R` preceded by `_dq_vivify(L)`, which
//!   creates the containers the paths of `L` need. The right-hand side is
//!   bound to a variable first, so it still sees the input as it was.
//!   jaq's native update then runs on a document it can walk.
//! - [`DEFS`] redefines `setpath`, `delpaths` and `del` the way jq defines
//!   them, on the natives in [`funs`]: `_dq_ensure` creates the
//!   containers a list of paths needs, `_dq_delpaths` deletes as jq's `delpaths`
//!   does — paths sorted, deleted together, a missing one skipped.
//!
//! What jaq refuses before a path exists stays refused: a slice of null
//! in a path (`.a[1:2] = …` on `{}`), and a fractional index.

use core::iter::once;
use std::collections::HashSet;

use jaq_core::box_iter::box_once;
use jaq_core::data::JustLut;
use jaq_core::load::lex::StrPart;
use jaq_core::load::parse::{BinaryOp, Def, Pattern, Term};
use jaq_core::native::{Fun, run, v};
use jaq_core::path::{Opt, Part, Path};
use jaq_core::{Cv, Error, ValT as _, ValX};
use jaq_json::{Map, Rc, Val};
use jaq_std::ValT as _;

/// Definitions chained after jaq's own, so these shadow jaq's.
///
/// `setpath` creates the path, then assigns through jaq's own `=`;
/// `delpaths` and `del` are jq's definitions.
pub(super) const DEFS: &str = r#"
def _dq_vivify(f): _dq_ensure([path(f)]);
def setpath($p; $x): _dq_ensure([$p]) | getpath($p) = $x;
def delpaths($ps): _dq_delpaths($ps);
def del(f): delpaths([path(f)]);
"#;

/// The definitions in [`DEFS`], parsed.
///
/// # Panics
///
/// Never: [`DEFS`] is a constant the tests compile.
pub(super) fn defs() -> impl Iterator<Item = Def<&'static str>> {
    jaq_core::load::parse(DEFS, |p| p.defs())
        .expect("the dq jq prelude parses")
        .into_iter()
}

type D = JustLut<Val>;

/// The natives [`DEFS`] calls.
pub(super) fn funs() -> impl Iterator<Item = Fun<D>> {
    let ensure: jaq_core::RunPtr<D> = |cv| {
        let (v, paths) = pop1(cv);
        box_once(ensure_all(v, &paths))
    };
    let delpaths: jaq_core::RunPtr<D> = |cv| {
        let (v, paths) = pop1(cv);
        box_once(delpaths(v, &paths))
    };
    [
        run::<D>(("_dq_ensure", v(1), ensure)),
        run::<D>(("_dq_delpaths", v(1), delpaths)),
    ]
    .into_iter()
}

/// The input and the one variable argument of a native.
fn pop1(mut cv: Cv<'_, D>) -> (Val, Val) {
    let arg = cv.0.pop_var();
    (cv.1, arg)
}

fn as_path<'a>(path: &Val) -> Result<&[Val], jaq_core::Exn<'a, Val>> {
    match path {
        Val::Arr(parts) => Ok(parts.as_slice()),
        other => Err(Error::str(format_args!("path must be an array, not {other}")).into()),
    }
}

// ---------------------------------------------------------------------------
// _dq_ensure
// ---------------------------------------------------------------------------

/// `v` with the containers each of `paths` (an array of paths) walks
/// through created, see [`ensure`].
///
/// One native call for all the paths of an assignment: the document is
/// owned here from the first path to the last, so a container copied for
/// the first path is not copied again for the next one.
fn ensure_all<'a>(mut v: Val, paths: &Val) -> ValX<'a, Val> {
    let Val::Arr(paths) = paths else {
        return Err(Error::str(format_args!("paths must be an array, not {paths}")).into());
    };
    for path in paths.iter() {
        v = ensure(v, as_path(path)?)?;
    }
    Ok(v)
}

/// `v` with the containers `path` walks through created: a null becomes
/// `{}` before a string key and `[]` before a number or a slice, and an
/// array is padded with nulls up to an index past its end — up to
/// [`MAX_INDEX`], past which it is jq's "Array index too large".
///
/// A missing object key at the end of the path is left for the assignment
/// to insert. What jq refuses (a string key into a number, say) is left as
/// it is: the assignment then fails the way jaq reports it.
fn ensure<'a>(v: Val, path: &[Val]) -> ValX<'a, Val> {
    if !needs_change(&v, path) {
        return Ok(v);
    }
    let Some((key, rest)) = path.split_first() else {
        return Ok(v);
    };
    match v {
        Val::Null => {
            let container = if is_str(key) {
                Val::obj(Map::default())
            } else {
                Val::Arr(Default::default())
            };
            ensure(container, path)
        }
        Val::Arr(mut items) => {
            let len = items.len();
            match array_index(key, len) {
                Some(idx) if idx > MAX_INDEX => Err(Error::str("Array index too large").into()),
                Some(idx) if idx >= len => {
                    // In place: the array is copied only when it is shared,
                    // so building one item by item stays linear.
                    Rc::make_mut(&mut items).resize(idx + 1, Val::Null);
                    ensure(Val::Arr(items), path)
                }
                _ => Val::Arr(items)
                    .map_index(key, Opt::Essential, |child| once(ensure(child, rest))),
            }
        }
        v @ Val::Obj(_) => v.map_index(key, Opt::Essential, |child| once(ensure(child, rest))),
        other => Ok(other),
    }
}

/// The largest index an assignment pads an array up to: jq's own limit.
/// Past it jq's `jv_array_set` fails with "Array index too large"
/// (`idx > (INT_MAX >> 2)`), where padding would otherwise allocate
/// without bound.
const MAX_INDEX: usize = 536_870_911;

/// Whether [`ensure`] has anything to create along `path`. A read-only
/// walk, so a path that exists costs no copy.
fn needs_change(v: &Val, path: &[Val]) -> bool {
    let Some((key, rest)) = path.split_first() else {
        return false;
    };
    match v {
        Val::Null => is_str(key) || int_key(key).is_some() || matches!(key, Val::Obj(_)),
        Val::Obj(map) if is_str(key) => map.get(key).map_or_else(
            || needs_change(&Val::Null, rest),
            |child| needs_change(child, rest),
        ),
        Val::Arr(items) => match array_index(key, items.len()) {
            Some(idx) => items.get(idx).is_none_or(|child| needs_change(child, rest)),
            None => false,
        },
        _ => false,
    }
}

fn is_str(key: &Val) -> bool {
    matches!(key, Val::TStr(_))
}

fn int_key(key: &Val) -> Option<isize> {
    match key {
        Val::Num(_) => key.as_isize(),
        _ => None,
    }
}

/// The position an integer `key` names in an array of `len` items,
/// counting a negative key from the end; `None` for a key that is not an
/// integer or lies before the start.
fn array_index(key: &Val, len: usize) -> Option<usize> {
    let i = int_key(key)?;
    if i >= 0 {
        usize::try_from(i).ok()
    } else {
        len.checked_sub(i.unsigned_abs())
    }
}

// ---------------------------------------------------------------------------
// _dq_delpaths
// ---------------------------------------------------------------------------

/// jq's `delpaths`: the paths are sorted and deleted together, so the
/// indices of one array all name its items before any deletion; a path
/// that does not exist, or runs through a null, is skipped.
fn delpaths<'a>(v: Val, paths: &Val) -> ValX<'a, Val> {
    let Val::Arr(paths) = paths else {
        return Err(Error::str(format_args!("paths must be an array, not {paths}")).into());
    };
    let mut sorted: Vec<&[Val]> = paths.iter().map(as_path).collect::<Result<_, _>>()?;
    sorted.sort();
    sorted.dedup();
    match sorted.first() {
        None => Ok(v),
        Some([]) => Ok(Val::Null),
        Some(_) => delete_sorted(v, &sorted, 0),
    }
}

/// Delete `paths` (sorted, all longer than `depth`, sharing their first
/// `depth` keys) from `v`, which sits at that common prefix.
fn delete_sorted<'a>(mut v: Val, paths: &[&[Val]], depth: usize) -> ValX<'a, Val> {
    let mut whole = Vec::new();
    let mut i = 0;
    while i < paths.len() {
        let key = &paths[i][depth];
        let group = paths[i..].iter().take_while(|p| &p[depth] == key).count();
        // Sorted, the shortest path of the group comes first: when it ends
        // here the whole key goes, and the longer ones go with it.
        if paths[i].len() == depth + 1 {
            whole.push(key.clone());
        } else {
            let child = child_at(&v, key)?;
            if !matches!(child, Val::Null) {
                let child = delete_sorted(child, &paths[i..i + group], depth + 1)?;
                v = v.map_index(key, Opt::Essential, |_| once(Ok(child.clone())))?;
            }
        }
        i += group;
    }
    delete_keys(v, &whole)
}

/// What a path step `key` reaches in `v` for a deletion further down:
/// null for a missing key or index, and an error for a key `v` cannot
/// have.
fn child_at<'a>(v: &Val, key: &Val) -> ValX<'a, Val> {
    match (v, key) {
        (Val::Null, _) => Ok(Val::Null),
        (Val::Obj(map), _) if is_str(key) => Ok(map.get(key).cloned().unwrap_or_default()),
        (Val::Arr(items), _) if int_key(key).is_some() => Ok(array_index(key, items.len())
            .and_then(|idx| items.get(idx))
            .cloned()
            .unwrap_or_default()),
        (Val::Arr(_), Val::Obj(bounds)) => {
            let bound = |name: &str| bounds.get(&Val::utf8_str(name.to_owned()));
            Ok(v.clone().range(bound("start")..bound("end"))?)
        }
        _ => Err(Error::str(format_args!("cannot index {v} with {key}")).into()),
    }
}

/// `v` without the object keys or array items `keys` name.
fn delete_keys<'a>(v: Val, keys: &[Val]) -> ValX<'a, Val> {
    if keys.is_empty() {
        return Ok(v);
    }
    match v {
        Val::Null => Ok(Val::Null),
        Val::Obj(map) => {
            if let Some(key) = keys.iter().find(|k| !is_str(k)) {
                return Err(Error::str(format_args!("cannot delete {key} from an object")).into());
            }
            let keys: HashSet<&Val> = keys.iter().collect();
            Ok(Val::obj(
                map.iter()
                    .filter(|(k, _)| !keys.contains(k))
                    .map(|(k, child)| (k.clone(), child.clone()))
                    .collect(),
            ))
        }
        Val::Arr(items) => {
            let mut dropped = vec![false; items.len()];
            for key in keys {
                for idx in array_positions(key, items.len())? {
                    dropped[idx] = true;
                }
            }
            Ok(items
                .iter()
                .zip(dropped)
                .filter(|(_, gone)| !gone)
                .map(|(item, _)| item.clone())
                .collect())
        }
        other => Err(Error::str(format_args!("cannot delete {} from {other}", keys[0])).into()),
    }
}

/// The positions in an array of `len` items that a deleted `key` names:
/// an index (none when out of range) or a slice.
fn array_positions<'a>(
    key: &Val,
    len: usize,
) -> Result<core::ops::Range<usize>, jaq_core::Exn<'a, Val>> {
    if int_key(key).is_some() {
        return Ok(array_index(key, len)
            .filter(|&idx| idx < len)
            .map_or(0..0, |idx| idx..idx + 1));
    }
    let Val::Obj(bounds) = key else {
        return Err(Error::str(format_args!("cannot delete {key} from an array")).into());
    };
    let bound = |name: &str, default: usize| -> Result<usize, jaq_core::Exn<'a, Val>> {
        match bounds.get(&Val::utf8_str(name.to_owned())) {
            None | Some(Val::Null) => Ok(default),
            Some(n) => {
                let i = int_key(n).ok_or_else(|| {
                    jaq_core::Exn::from(Error::str(format_args!(
                        "slice bound {n} is not an integer"
                    )))
                })?;
                let from_start = if i >= 0 {
                    usize::try_from(i).unwrap_or(usize::MAX)
                } else {
                    len.saturating_sub(i.unsigned_abs())
                };
                Ok(from_start.min(len))
            }
        }
    };
    let start = bound("start", 0)?;
    let end = bound("end", len)?;
    Ok(start..end.max(start))
}

// ---------------------------------------------------------------------------
// rewrite
// ---------------------------------------------------------------------------

/// The variable the right-hand side of a rewritten assignment is bound to.
const RHS: &str = "$__dq_rhs";

/// `code` with each assignment `L op R` preceded by `_dq_vivify(L)`, or
/// `None` when `code` holds no assignment operator or does not parse as a
/// single term (a module header, say — jaq reports that itself).
pub(super) fn rewrite(code: &str) -> Option<String> {
    let term = jaq_core::load::parse(code, |p| p.term())?;
    if !has_assignment(&term) {
        return None;
    }
    let mut out = String::with_capacity(code.len() * 2);
    print_term(&term, &mut out);
    Some(out)
}

fn has_assignment(t: &Term<&str>) -> bool {
    let mut found = false;
    visit(t, &mut |t| {
        if let Term::BinOp(_, op, _) = t {
            found |= matches!(
                op,
                BinaryOp::Assign | BinaryOp::Update | BinaryOp::UpdateMath(_) | BinaryOp::UpdateAlt
            );
        }
    });
    found
}

/// Call `f` on `t` and on every term inside it.
fn visit<'s>(t: &Term<&'s str>, f: &mut impl FnMut(&Term<&'s str>)) {
    f(t);
    match t {
        Term::Id | Term::Recurse | Term::Num(_) | Term::Var(_) | Term::Break(_) => {}
        Term::Str(_, parts) => {
            for part in parts {
                if let StrPart::Term(t) = part {
                    visit(t, f);
                }
            }
        }
        Term::Arr(t) => {
            if let Some(t) = t {
                visit(t, f);
            }
        }
        Term::Obj(entries) => {
            for (k, v) in entries {
                visit(k, f);
                if let Some(v) = v {
                    visit(v, f);
                }
            }
        }
        Term::Neg(t) | Term::Label(_, t) => visit(t, f),
        Term::BinOp(l, op, r) => {
            visit(l, f);
            if let BinaryOp::Pipe(Some(p)) = op {
                visit_pattern(p, f);
            }
            visit(r, f);
        }
        Term::Fold(_, xs, p, args) => {
            visit(xs, f);
            visit_pattern(p, f);
            for a in args {
                visit(a, f);
            }
        }
        Term::TryCatch(t, c) => {
            visit(t, f);
            if let Some(c) = c {
                visit(c, f);
            }
        }
        Term::IfThenElse(branches, els) => {
            for (c, t) in branches {
                visit(c, f);
                visit(t, f);
            }
            if let Some(e) = els {
                visit(e, f);
            }
        }
        Term::Def(defs, t) => {
            for d in defs {
                visit(&d.body, f);
            }
            visit(t, f);
        }
        Term::Call(_, args) => {
            for a in args {
                visit(a, f);
            }
        }
        Term::Path(t, path) => {
            visit(t, f);
            for (part, _) in &path.0 {
                match part {
                    Part::Index(i) => visit(i, f),
                    Part::Range(from, to) => {
                        for b in [from, to].into_iter().flatten() {
                            visit(b, f);
                        }
                    }
                }
            }
        }
    }
}

fn visit_pattern<'s>(p: &Pattern<&'s str>, f: &mut impl FnMut(&Term<&'s str>)) {
    match p {
        Pattern::Var(_) => {}
        Pattern::Arr(items) => {
            for item in items {
                visit_pattern(item, f);
            }
        }
        Pattern::Obj(entries) => {
            for (k, v) in entries {
                visit(k, f);
                visit_pattern(v, f);
            }
        }
    }
}

/// Print `t` as jq source, every sub-term parenthesised, every assignment
/// rewritten.
fn print_term(t: &Term<&str>, out: &mut String) {
    match t {
        Term::Id => out.push('.'),
        Term::Recurse => out.push_str(".."),
        Term::Num(n) | Term::Var(n) => out.push_str(n),
        Term::Str(fmt, parts) => print_str(*fmt, parts, out),
        Term::Arr(None) => out.push_str("[]"),
        Term::Arr(Some(t)) => {
            out.push('[');
            paren(t, out);
            out.push(']');
        }
        Term::Obj(entries) => {
            out.push('{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match k {
                    Term::Var(_) | Term::Str(..) => print_term(k, out),
                    _ => paren(k, out),
                }
                if let Some(v) = v {
                    out.push_str(": ");
                    paren(v, out);
                }
            }
            out.push('}');
        }
        Term::Neg(t) => {
            out.push('-');
            paren(t, out);
        }
        Term::BinOp(l, op, r) => print_binop(l, op, r, out),
        Term::Label(x, t) => {
            out.push_str("label ");
            out.push_str(x);
            out.push_str(" | ");
            paren(t, out);
        }
        Term::Break(x) => {
            out.push_str("break ");
            out.push_str(x);
        }
        Term::Fold(kind, xs, p, args) => {
            out.push_str(kind);
            out.push(' ');
            paren(xs, out);
            out.push_str(" as ");
            print_pattern(p, out);
            print_args(args, out);
        }
        Term::TryCatch(t, c) => {
            out.push_str("try ");
            paren(t, out);
            if let Some(c) = c {
                out.push_str(" catch ");
                paren(c, out);
            }
        }
        Term::IfThenElse(branches, els) => {
            for (i, (c, t)) in branches.iter().enumerate() {
                out.push_str(if i == 0 { "if " } else { " elif " });
                paren(c, out);
                out.push_str(" then ");
                paren(t, out);
            }
            if let Some(e) = els {
                out.push_str(" else ");
                paren(e, out);
            }
            out.push_str(" end");
        }
        Term::Def(defs, t) => {
            for d in defs {
                out.push_str("def ");
                out.push_str(d.name);
                if !d.args.is_empty() {
                    out.push('(');
                    out.push_str(&d.args.join("; "));
                    out.push(')');
                }
                out.push_str(": ");
                paren(&d.body, out);
                out.push_str("; ");
            }
            paren(t, out);
        }
        Term::Call(name, args) => {
            out.push_str(name);
            print_args(args, out);
        }
        Term::Path(t, path) => {
            match **t {
                Term::Id => out.push('.'),
                _ => paren(t, out),
            }
            print_path(path, out);
        }
    }
}

fn paren(t: &Term<&str>, out: &mut String) {
    out.push('(');
    print_term(t, out);
    out.push(')');
}

fn print_args(args: &[Term<&str>], out: &mut String) {
    if args.is_empty() {
        return;
    }
    out.push('(');
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        paren(a, out);
    }
    out.push(')');
}

fn print_binop(l: &Term<&str>, op: &BinaryOp<&str>, r: &Term<&str>, out: &mut String) {
    let infix = |l: &Term<&str>, op: &str, r: &Term<&str>, out: &mut String| {
        paren(l, out);
        out.push(' ');
        out.push_str(op);
        out.push(' ');
        paren(r, out);
    };
    // `L op R` becomes `(R) as $rhs | _dq_vivify(L) | (L) op $rhs`: the
    // right-hand side sees the input before any path is created, as it
    // sees it in jq. `L |= F` needs no binding: F sees the value at the
    // path.
    let assign = |op: &str, out: &mut String| {
        paren(r, out);
        out.push_str(" as ");
        out.push_str(RHS);
        out.push_str(" | _dq_vivify");
        paren(l, out);
        out.push_str(" | ");
        paren(l, out);
        out.push(' ');
        out.push_str(op);
        out.push(' ');
        out.push_str(RHS);
    };
    match op {
        BinaryOp::Pipe(None) => infix(l, "|", r, out),
        BinaryOp::Pipe(Some(p)) => {
            paren(l, out);
            out.push_str(" as ");
            print_pattern(p, out);
            out.push_str(" | ");
            paren(r, out);
        }
        BinaryOp::Comma => infix(l, ",", r, out),
        BinaryOp::Alt => infix(l, "//", r, out),
        BinaryOp::Or => infix(l, "or", r, out),
        BinaryOp::And => infix(l, "and", r, out),
        BinaryOp::Math(m) => infix(l, m.as_str(), r, out),
        BinaryOp::Cmp(c) => infix(l, c.as_str(), r, out),
        BinaryOp::Assign => assign("=", out),
        BinaryOp::UpdateMath(m) => assign(&format!("{}=", m.as_str()), out),
        BinaryOp::UpdateAlt => assign("//=", out),
        BinaryOp::Update => {
            out.push_str("_dq_vivify");
            paren(l, out);
            out.push_str(" | ");
            infix(l, "|=", r, out);
        }
    }
}

fn print_pattern(p: &Pattern<&str>, out: &mut String) {
    match p {
        Pattern::Var(x) => out.push_str(x),
        Pattern::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                print_pattern(item, out);
            }
            out.push(']');
        }
        Pattern::Obj(entries) => {
            out.push('{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                paren(k, out);
                out.push_str(": ");
                print_pattern(v, out);
            }
            out.push('}');
        }
    }
}

fn print_path(path: &Path<Term<&str>>, out: &mut String) {
    for (part, opt) in &path.0 {
        out.push('[');
        match part {
            Part::Index(i) => paren(i, out),
            Part::Range(from, to) => {
                if let Some(from) = from {
                    paren(from, out);
                }
                if from.is_some() || to.is_some() {
                    out.push(':');
                }
                if let Some(to) = to {
                    paren(to, out);
                }
            }
        }
        out.push(']');
        if matches!(opt, Opt::Optional) {
            out.push('?');
        }
    }
}

/// Print a string literal: the parts without escapes as they were, an
/// escaped character re-escaped where it has to be, an interpolation
/// rewritten.
fn print_str(fmt: Option<&str>, parts: &[StrPart<&str, Term<&str>>], out: &mut String) {
    if let Some(fmt) = fmt {
        out.push_str(fmt);
        out.push(' ');
    }
    out.push('"');
    for part in parts {
        match part {
            StrPart::Str(s) => out.push_str(s),
            StrPart::Char('"') => out.push_str("\\\""),
            StrPart::Char('\\') => out.push_str("\\\\"),
            StrPart::Char(c) => out.push(*c),
            StrPart::Term(t) => {
                out.push_str("\\(");
                print_term(t, out);
                out.push(')');
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::rewrite;
    use crate::JqEngine;
    use pretty_assertions::assert_eq;

    /// The compact JSON every output of `filter` on `input` is.
    fn run(filter: &str, input: &str) -> Result<Vec<String>, String> {
        let engine = JqEngine::compile(filter).map_err(|e| e.to_string())?;
        let input: serde_json::Value = serde_json::from_str(input).expect("test input is JSON");
        engine
            .run(&input)
            .map(|outs| outs.iter().map(ToString::to_string).collect())
            .map_err(|e| e.to_string())
    }

    fn one(filter: &str, input: &str) -> String {
        let outs = run(filter, input).unwrap_or_else(|e| panic!("{filter} on {input}: {e}"));
        assert_eq!(outs.len(), 1, "{filter} on {input}: {outs:?}");
        outs.into_iter().next().unwrap_or_default()
    }

    #[test]
    fn assignment_operators_create_what_the_path_walks_through() {
        // Each expectation is what jq 1.7 prints for the same program.
        for (filter, input, want) in [
            (".a.b = 1", "{}", r#"{"a":{"b":1}}"#),
            (".a.b |= 1", "{}", r#"{"a":{"b":1}}"#),
            (".a.b += 1", "{}", r#"{"a":{"b":1}}"#),
            (".a.b //= 1", "{}", r#"{"a":{"b":1}}"#),
            (".a.b *= 2", r#"{"a":{"b":3}}"#, r#"{"a":{"b":6}}"#),
            (".a[0] = 1", "{}", r#"{"a":[1]}"#),
            (".a[2] = 1", "{}", r#"{"a":[null,null,1]}"#),
            (".a = 1", "null", r#"{"a":1}"#),
            (".[1] = 1", "null", "[null,1]"),
            (".a.b = 1", r#"{"a":null}"#, r#"{"a":{"b":1}}"#),
            (".a[0].b |= 3", "null", r#"{"a":[{"b":3}]}"#),
            ("[[1]] | .[0][2] = 5", "null", "[[1,null,5]]"),
            ("[1] | .[3] |= .", "null", "[1,null,null,null]"),
            ("[3] | .[-1] += 1", "null", "[4]"),
            ("(.a, .b) = 1", "{}", r#"{"a":1,"b":1}"#),
            // The right-hand side sees the input as it was.
            (".a.b = .a", "{}", r#"{"a":{"b":null}}"#),
            (r#".a."x-y" = 1"#, "{}", r#"{"a":{"x-y":1}}"#),
            ("def f(g): g |= 3; f(.a.b)", "{}", r#"{"a":{"b":3}}"#),
            (
                r#"reduce ("a", "b") as $k (.; .[$k].z = 1)"#,
                "{}",
                r#"{"a":{"z":1},"b":{"z":1}}"#,
            ),
            ("[1,2,3] | .[1:] = [\"x\"]", "null", r#"[1,"x"]"#),
        ] {
            assert_eq!(one(filter, input), want, "{filter} on {input}");
        }
    }

    #[test]
    fn several_right_hand_outputs_give_several_results() {
        assert_eq!(
            run(".a.b = (1, 2)", "{}"),
            Ok(vec![
                r#"{"a":{"b":1}}"#.to_owned(),
                r#"{"a":{"b":2}}"#.to_owned()
            ])
        );
    }

    #[test]
    fn setpath_delpaths_and_del_follow_jq() {
        for (filter, input, want) in [
            (r#"setpath(["a", "b"]; 1)"#, "{}", r#"{"a":{"b":1}}"#),
            (r#"setpath(["a", 0, "b"]; 1)"#, "{}", r#"{"a":[{"b":1}]}"#),
            ("del(.a.b)", "{}", "{}"),
            ("del(.a[0])", "{}", "{}"),
            ("del(.a)", "null", "null"),
            (r#"delpaths([["a", "b"], ["c"]])"#, "{}", "{}"),
            // Deleted together: both indices name the items before deletion.
            ("del(.[0], .[1])", "[1,2,3]", "[3]"),
            ("del(.[5])", "[1,2,3]", "[1,2,3]"),
            ("del(.[-1])", "[1,2]", "[1]"),
            ("del(.[-5])", "[1,2]", "[1,2]"),
            ("del(.a[0, 2])", r#"{"a":[1,2,3]}"#, r#"{"a":[2]}"#),
            ("del(.a[1:3])", r#"{"a":[1,2,3,4]}"#, r#"{"a":[1,4]}"#),
            ("del(.[] | select(. == 2))", "[1,2,3,2]", "[1,3]"),
            (".a |= del(.b)", r#"{"a":{"b":1}}"#, r#"{"a":{}}"#),
            ("delpaths([[]])", r#"{"a":1}"#, "null"),
        ] {
            assert_eq!(one(filter, input), want, "{filter} on {input}");
        }
    }

    #[test]
    fn paths_jq_refuses_stay_refused() {
        for (filter, input, message) in [
            (".a.b = 1", r#"{"a":"x"}"#, "cannot index"),
            (".a.b = 1", r#"{"a":1}"#, "cannot index"),
            ("del(.a.b)", r#"{"a":"x"}"#, "cannot index"),
            (".a[] = 1", "{}", "cannot use null as iterable"),
            (".[-1] = 1", "[]", "out of bounds"),
            (".a.b -= 1", "{}", "null - 1"),
            (r#"setpath("a"; 1)"#, "{}", "path must be an array"),
            ("delpaths(1)", "{}", "paths must be an array"),
            (r#"delpaths([["a"]])"#, r#""s""#, "cannot delete"),
        ] {
            let err = run(filter, input).expect_err(filter);
            assert!(err.contains(message), "{filter} on {input}: {err}");
        }
    }

    #[test]
    fn an_index_past_jqs_limit_is_an_error_not_an_allocation() {
        // jq refuses an index past 536870911 with "Array index too large".
        for (filter, input) in [
            (".[536870912] = 1", "null"),
            (".[4000000000000000000] = 1", "null"),
            (".a[.n] = 1", r#"{"n":4000000000000000000}"#),
            ("[1] | .[536870912] |= 1", "null"),
            ("setpath([536870912]; 1)", "null"),
        ] {
            let err = run(filter, input).expect_err(filter);
            assert!(
                err.contains("Array index too large"),
                "{filter} on {input}: {err}"
            );
        }
    }

    #[test]
    fn building_an_array_by_index_is_linear() {
        // Padding copied the whole array on every append: 40k items took
        // ten seconds (jq: 0.03 s). In place, 100k take well under one,
        // even unoptimised; copying, they would take minutes.
        let start = std::time::Instant::now();
        assert_eq!(
            one(
                "reduce range(100000) as $i ([]; .[$i] = $i) | length",
                "null"
            ),
            "100000"
        );
        let took = start.elapsed();
        assert!(took.as_secs() < 15, "took {took:?}");
    }

    #[test]
    fn rewrite_leaves_a_program_without_assignment_alone() {
        assert_eq!(rewrite(".a | map(. + 1)"), None);
        assert_eq!(rewrite("path(.a.b)"), None);
        // Not a single term: jaq loads (and refuses) it itself.
        assert_eq!(rewrite(r#"include "x"; .a = 1"#), None);
    }

    #[test]
    fn rewrite_puts_vivify_before_each_assignment() {
        assert_eq!(
            rewrite(".a = 1").as_deref(),
            Some("(1) as $__dq_rhs | _dq_vivify(.[(\"a\")]) | (.[(\"a\")]) = $__dq_rhs")
        );
        assert_eq!(
            rewrite(".a |= . + 1").as_deref(),
            Some("_dq_vivify(.[(\"a\")]) | (.[(\"a\")]) |= ((.) + (1))")
        );
    }

    #[test]
    fn rewritten_programs_compute_what_the_originals_do() {
        // Every syntactic form the printer handles, around an assignment
        // that needs no path created: the rewritten program must compile
        // and give the original's result.
        let input = r#"{"a":{"b":1,"c":[1,2,3]},"s":"x\"y"}"#;
        for filter in [
            r#".a.b = 2 | .s, -(.a.b), .a.c[1:], .a.c[:2], .a.c[1:2], .a.c[]?, .a?.b"#,
            r#".a.b |= . * 10 | [.a.b, ..] | length"#,
            r#"1 as $x | .a.b += 1 | {x: .a.b, "y": 2, (.s): 3, $x} | keys"#,
            r#". as {a: {b: $b}, s: $s} | .a.b //= $b | [$b, $s]"#,
            r#"[.a.c[0], .s] as [$x, $y] | .a.b = $x | [.a.b, $y]"#,
            r#"if .a.b == 1 then .a.b = 2 elif .s then . else empty end | .a.b"#,
            r#"try (.a.b -= 1) catch "no" | .a.b"#,
            r#"reduce .a.c[] as $i (.; .a.b += $i) | .a.b"#,
            r#"[foreach .a.c[] as $i (0; . + $i; .)] as $sums | .a.b = $sums | .a.b"#,
            r#"label $out | .a.b /= 2 | ., break $out | .a.b"#,
            r#"def inc(f): f |= . + 1; def two: 2; inc(.a.b) | .a.b %= two | .a.b"#,
            r#".a.b = "\(.s)-\(.a.b)\n\té\\" | .a.b"#,
            r#".a.b = @base64 "\(.s)" | .a.b | @base64d"#,
            r#".a.c[1] = (.a.c | length) | .a.c and (.a.b or false), (.a.b < 2), (.a.b != 1)"#,
            r#".a["b"] = [.a.c[] | select(. > 1)] | .a.b"#,
            r#".[.s] = 1 | to_entries | map(.key)"#,
        ] {
            let rewritten = rewrite(filter).unwrap_or_else(|| panic!("{filter} not rewritten"));
            assert_eq!(
                run(&rewritten, input),
                run(filter, input),
                "{filter}\nrewritten: {rewritten}"
            );
            assert!(
                run(filter, input).is_ok(),
                "{filter}: {:?}",
                run(filter, input)
            );
        }
    }
}
