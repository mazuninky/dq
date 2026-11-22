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
//!   creates the containers the paths of `L` need, and every index on `L`
//!   through `_dq_index`, which takes a float with an integral value as the
//!   integer it is (jaq's update refuses a float). The right-hand side is
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
use jaq_core::path::{Opt, Part};
use jaq_core::{Cv, Error, ValT as _, ValX};
use jaq_json::{Map, Num, Rc, Val};
use jaq_std::ValT as _;

use super::keys;

/// Definitions chained after jaq's own, so these shadow jaq's.
///
/// `setpath` creates the path, then assigns through jaq's own `=` (an
/// integral float in the path taken as the integer, as jq takes it);
/// `delpaths` and `del` are jq's definitions. `getpath`, `has`,
/// `from_entries` and `pick` refuse a non-string object key, as jq does
/// (see [`super::keys`]); `in` and `with_entries` are repeated so they call
/// these and not jaq's. `from_entries` takes the key and value names jq
/// 1.8 takes. `halt_error` does not write its input to the log: to call it
/// is an error (see `output` in [`super`]).
pub(super) const DEFS: &str = r#"
def _dq_vivify(f): _dq_ensure([path(f)]);
def getpath($path): reduce $path[] as $p (.; _dq_objkey($p) | .[$p | _dq_rindex]);
def setpath($p; $x):
  _dq_ensure([$p])
  | ($p | map(_dq_index)) as $p
  | reduce $p[] as $k (.; .[$k]) = $x;
def delpaths($ps): _dq_delpaths($ps);
def del(f): delpaths([path(f)]);
def has($k): _dq_has($k);
def in(xs): . as $x | xs | has($x);
def pick(f): . as $top | reduce path(f) as $p (null; setpath($p; $top | getpath($p)));
def from_entries: reduce .[] as $x ({};
  . + { ($x | .key // .Key // .name // .Name | _dq_key):
        ($x | if has("value") then .value else .Value end) });
def with_entries(f): to_entries | map(f) | from_entries;
def halt_error($exit_code): halt($exit_code);
def halt_error: halt;
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

/// The natives [`DEFS`] and the rewritten programs ([`rewrite`]) call.
pub(super) fn funs() -> impl Iterator<Item = Fun<D>> {
    let ensure: jaq_core::RunPtr<D> = |cv| {
        let (v, paths) = pop1(cv);
        box_once(ensure_all(v, &paths))
    };
    let delpaths: jaq_core::RunPtr<D> = |cv| {
        let (v, paths) = pop1(cv);
        box_once(delpaths(v, &paths))
    };
    let index: jaq_core::RunPtr<D> = |cv| box_once(Ok(integral(cv.1)));
    let read_index: jaq_core::RunPtr<D> = |cv| box_once(Ok(read_index(cv.1)));
    [
        run::<D>(("_dq_ensure", v(1), ensure)),
        run::<D>(("_dq_delpaths", v(1), delpaths)),
        run::<D>(("_dq_index", v(0), index)),
        run::<D>(("_dq_rindex", v(0), read_index)),
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
/// to insert. A key jq refuses for the value it indexes (a number into an
/// object or a string, say) is jq's "Cannot index … with …", the key as
/// written.
fn ensure<'a>(v: Val, path: &[Val]) -> ValX<'a, Val> {
    if !needs_change(&v, path) {
        return Ok(v);
    }
    let Some((key, rest)) = path.split_first() else {
        return Ok(v);
    };
    keys::check_index(&v, key)?;
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
                _ => Val::Arr(items).map_index(&integral(key.clone()), Opt::Essential, |child| {
                    once(ensure(child, rest))
                }),
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
        // Not a change: `ensure` refuses the key.
        _ if keys::check_index(v, key).is_err() => true,
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

/// The integer a number `key` is: an integer, or a float with an integral
/// value (`2.0`, `1e3`), as jq takes it. One too large for `isize` comes
/// back saturated, so it is past [`MAX_INDEX`] rather than not an index.
/// `None` for anything else — a fractional, infinite or NaN number, which
/// jaq refuses as an index.
fn int_key(key: &Val) -> Option<isize> {
    if !matches!(key, Val::Num(_)) {
        return None;
    }
    if let Some(i) = key.as_isize() {
        return Some(i);
    }
    let f = key.as_f64()?;
    // `as` saturates; a big integer comes here as its float.
    (f.is_finite() && f.fract() == 0.0).then_some(f as isize)
}

/// `v`, or the integer it is when it is a float with an integral value in
/// `isize` range: what `_dq_index` gives an index on the left-hand side
/// of an assignment, which jaq's update would refuse as a float. A
/// positive one past that range (`1e300`, `100000000000000000000`) is
/// `isize::MAX`: into an array that is past [`MAX_INDEX`], jq's "Array
/// index too large", where jaq would say it cannot index the array with
/// a float.
fn integral(v: Val) -> Val {
    if !matches!(v, Val::Num(Num::Float(_) | Num::Dec(_))) {
        return v;
    }
    match v.as_f64() {
        // `isize::MAX as f64` rounds up to 2^63, hence `<`.
        Some(f) if f.fract() == 0.0 && f >= isize::MIN as f64 && f < isize::MAX as f64 => {
            Val::Num(Num::Int(f as isize))
        }
        Some(f) if f.is_finite() && f.fract() == 0.0 && f > 0.0 => Val::Num(Num::Int(isize::MAX)),
        _ => v,
    }
}

/// `v` as an index that reads, outside an assignment, what jq reads: a
/// number truncated toward zero (`1.7` is `1`, `-1.5` is `-1`), one no
/// position holds — NaN, infinite, past `isize` either way (`-1e300`) —
/// as `isize::MAX`, which reads nothing, as jq's `null`. jaq refuses a
/// fractional index, and a float past `isize`. Anything else is left as
/// it is: an object refuses a number before this (`_dq_objkey`), and null
/// reads nothing at any index.
fn read_index(v: Val) -> Val {
    if !matches!(v, Val::Num(Num::Float(_) | Num::Dec(_))) {
        return v;
    }
    let Some(f) = v.as_f64() else {
        return v;
    };
    let t = f.trunc();
    // `isize::MAX as f64` rounds up to 2^63, hence `<`; NaN fails both.
    if t >= isize::MIN as f64 && t < isize::MAX as f64 {
        Val::Num(Num::Int(t as isize))
    } else {
        Val::Num(Num::Int(isize::MAX))
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
        (Val::Obj(_), _) => Err(keys::index_error(v, key).into()),
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
                return Err(Error::str(format_args!(
                    "Cannot delete {} field of object",
                    keys::type_name(key)
                ))
                .into());
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
        return Err(Error::str(format_args!(
            "Cannot delete {} element of array",
            keys::type_name(key)
        ))
        .into());
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

/// The variable the right-hand side of a rewritten assignment is bound to,
/// with a number appended when `code` already holds the name: the
/// left-hand side is evaluated inside the binding, so a variable of the
/// program by that name would be captured.
fn rhs_var(code: &str) -> String {
    fresh_var(code, "$__dq_rhs")
}

/// `base`, with a number appended until `code` does not hold it.
fn fresh_var(code: &str, base: &str) -> String {
    let mut name = base.to_owned();
    let mut n = 0_u32;
    while code.contains(name.as_str()) {
        n += 1;
        name = format!("{base}{n}");
    }
    name
}

/// `code` rewritten, or `None` when it needs no rewrite or does not parse
/// as a single term (a module header, say — jaq reports that itself).
///
/// The rewrite precedes each assignment `L op R` with `_dq_vivify(L)`, and
/// checks each index that is not a string literal, and each computed
/// object key, against jq's rule that an object key is a string (see
/// [`super::keys`]).
pub(super) fn rewrite(code: &str) -> Option<String> {
    let term = jaq_core::load::parse(code, |p| p.term())?;
    if !needs_rewrite(&term) {
        return None;
    }
    let mut printer = Printer {
        out: String::with_capacity(code.len() * 2),
        rhs: rhs_var(code),
        key: fresh_var(code, "$__dq_key"),
        keys: 0,
        lhs: false,
        update: false,
    };
    printer.term(&term);
    Some(printer.out)
}

/// Whether `t` holds an assignment, an index that is not a string literal
/// or a computed object key.
fn needs_rewrite(t: &Term<&str>) -> bool {
    let mut found = false;
    visit(t, &mut |t| match t {
        Term::BinOp(_, op, _) => {
            found |= matches!(
                op,
                BinaryOp::Assign | BinaryOp::Update | BinaryOp::UpdateMath(_) | BinaryOp::UpdateAlt
            );
        }
        Term::Path(_, path) => {
            found |= path
                .0
                .iter()
                .any(|(part, _)| matches!(part, Part::Index(i) if checked_index(i)));
        }
        Term::Obj(entries) => found |= entries.iter().any(|(k, v)| computed_key(k, v.as_ref())),
        _ => {}
    });
    found
}

/// Whether an index goes through `_dq_objkey`: anything but a string,
/// which an object takes as it is.
fn checked_index(i: &Term<&str>) -> bool {
    !matches!(i, Term::Str(..))
}

/// Whether an object-construction key `k` is computed, so it goes through
/// `_dq_key`: a string literal or `{$x}` (the key `"x"`) is a string.
fn computed_key(k: &Term<&str>, v: Option<&Term<&str>>) -> bool {
    match k {
        Term::Str(..) => false,
        Term::Var(_) => v.is_some(),
        _ => true,
    }
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

/// Prints a term back as jq source, every sub-term parenthesised, every
/// assignment rewritten.
struct Printer {
    out: String,
    /// The variable the right-hand side of an assignment is bound to: one
    /// the program does not hold ([`rhs_var`]).
    rhs: String,
    /// The prefix of the variables a checked index is bound to: one the
    /// program does not hold, so no name with a number appended is in it.
    key: String,
    /// The number of index variables made so far.
    keys: usize,
    /// Printing the left-hand side the update itself runs on, after
    /// `_dq_vivify` walked it in path mode. A native called there would
    /// copy the whole document: jaq keeps the value a native's arguments
    /// were bound on alive while the update runs, so `Rc::make_mut`
    /// finds it shared. `_dq_vivify` has already refused a non-string
    /// object key on the way, so an index needs no check here, and `.[k]?`
    /// is checked with `if`, which jaq evaluates before the update.
    update: bool,
    /// Printing the left-hand side of an assignment: each index on its
    /// paths goes through `_dq_index`, so an integral float (`.[2.0]`)
    /// indexes an array as the integer it is, as in jq — jaq's update
    /// refuses a float index.
    lhs: bool,
}

impl Printer {
    fn term(&mut self, t: &Term<&str>) {
        match t {
            Term::Id => self.out.push('.'),
            Term::Recurse => self.out.push_str(".."),
            Term::Num(n) | Term::Var(n) => self.out.push_str(n),
            Term::Str(fmt, parts) => self.str_lit(*fmt, parts),
            Term::Arr(None) => self.out.push_str("[]"),
            Term::Arr(Some(t)) => {
                self.out.push('[');
                self.paren(t);
                self.out.push(']');
            }
            Term::Obj(entries) => {
                self.out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    if computed_key(k, v.as_ref()) {
                        self.out.push_str("((");
                        self.term(k);
                        self.out.push_str(") | _dq_key)");
                    } else {
                        self.term(k);
                    }
                    if let Some(v) = v {
                        self.out.push_str(": ");
                        self.paren(v);
                    }
                }
                self.out.push('}');
            }
            Term::Neg(t) => {
                self.out.push('-');
                self.paren(t);
            }
            Term::BinOp(l, op, r) => self.binop(l, op, r),
            Term::Label(x, t) => {
                self.out.push_str("label ");
                self.out.push_str(x);
                self.out.push_str(" | ");
                self.paren(t);
            }
            Term::Break(x) => {
                self.out.push_str("break ");
                self.out.push_str(x);
            }
            Term::Fold(kind, xs, p, args) => {
                self.out.push_str(kind);
                self.out.push(' ');
                self.paren(xs);
                self.out.push_str(" as ");
                self.pattern(p);
                self.args(args);
            }
            Term::TryCatch(t, c) => {
                self.out.push_str("try ");
                self.paren(t);
                if let Some(c) = c {
                    self.out.push_str(" catch ");
                    self.paren(c);
                }
            }
            Term::IfThenElse(branches, els) => {
                for (i, (c, t)) in branches.iter().enumerate() {
                    self.out.push_str(if i == 0 { "if " } else { " elif " });
                    self.paren(c);
                    self.out.push_str(" then ");
                    self.paren(t);
                }
                if let Some(e) = els {
                    self.out.push_str(" else ");
                    self.paren(e);
                }
                self.out.push_str(" end");
            }
            Term::Def(defs, t) => {
                for d in defs {
                    self.out.push_str("def ");
                    self.out.push_str(d.name);
                    if !d.args.is_empty() {
                        self.out.push('(');
                        self.out.push_str(&d.args.join("; "));
                        self.out.push(')');
                    }
                    self.out.push_str(": ");
                    // A definition's body is not the left-hand side of the
                    // assignment it may sit in.
                    let lhs = core::mem::replace(&mut self.lhs, false);
                    let update = core::mem::replace(&mut self.update, false);
                    self.paren(&d.body);
                    self.lhs = lhs;
                    self.update = update;
                    self.out.push_str("; ");
                }
                self.paren(t);
            }
            Term::Call(name, args) => {
                self.out.push_str(name);
                self.args(args);
            }
            Term::Path(t, path) => self.path_term(t, &path.0),
        }
    }

    /// `t` followed by `parts`. The last index that is not a string
    /// literal, `t[...][e]`, becomes
    /// `((e) as $k | t[...] | _dq_objkey($k) | .[$k])`: `e` sees the
    /// input `t` sees, as in jq, and the check sees the value indexed.
    /// A number literal or a variable needs no binding. The parts after it
    /// follow as they are.
    fn path_term(&mut self, t: &Term<&str>, parts: &[(Part<Term<&str>>, Opt)]) {
        let checked = parts
            .iter()
            .rposition(|(part, _)| matches!(part, Part::Index(i) if checked_index(i)));
        let Some(at) = checked else {
            match t {
                Term::Id => self.out.push('.'),
                _ => self.paren(t),
            }
            self.parts(parts);
            return;
        };
        let (Part::Index(i), opt) = &parts[at] else {
            unreachable!("rposition found an index");
        };
        if self.lhs && self.update && matches!(opt, Opt::Essential) {
            // `_dq_vivify` checked this key already.
            match t {
                Term::Id => self.out.push('.'),
                _ => self.paren(t),
            }
            self.parts(parts);
            return;
        }
        self.out.push('(');
        let key = match i {
            Term::Num(n) | Term::Var(n) => (*n).to_owned(),
            _ => {
                let key = format!("{}_{}", self.key, self.keys);
                self.keys += 1;
                // The key expression is not on the left-hand side: it
                // computes a value, not a path.
                let lhs = core::mem::replace(&mut self.lhs, false);
                self.paren(i);
                self.lhs = lhs;
                self.out.push_str(" as ");
                self.out.push_str(&key);
                self.out.push_str(" | ");
                key
            }
        };
        self.out.push('(');
        self.path_term(t, &parts[..at]);
        self.out.push_str(") | ");
        if self.lhs && self.update {
            self.out.push_str("if _dq_badkey(");
            self.out.push_str(&key);
            self.out.push_str(") then empty else ");
        } else {
            self.out.push_str(match opt {
                Opt::Optional => "_dq_objkey_opt(",
                Opt::Essential => "_dq_objkey(",
            });
            self.out.push_str(&key);
            self.out.push_str(") | ");
        }
        // An integral float indexes an array as the integer it is, read or
        // assigned, as in jq; a read truncates any other number and reads
        // nothing past `isize` ([`read_index`]). The check above saw the
        // key as written.
        self.out.push_str(".[(");
        self.out.push_str(&key);
        self.out.push_str(if self.lhs {
            " | _dq_index)"
        } else {
            " | _dq_rindex)"
        });
        self.out.push(']');
        if matches!(opt, Opt::Optional) {
            self.out.push('?');
        }
        if self.lhs && self.update {
            self.out.push_str(" end");
        }
        self.out.push(')');
        self.parts(&parts[at + 1..]);
    }

    fn paren(&mut self, t: &Term<&str>) {
        self.out.push('(');
        self.term(t);
        self.out.push(')');
    }

    /// `t`, printed as the left-hand side of an assignment, as
    /// `_dq_vivify` walks it (`update` false) or as the update runs on it.
    fn lhs(&mut self, t: &Term<&str>, update: bool) {
        let lhs = core::mem::replace(&mut self.lhs, true);
        let was = core::mem::replace(&mut self.update, update);
        self.paren(t);
        self.lhs = lhs;
        self.update = was;
    }

    fn args(&mut self, args: &[Term<&str>]) {
        if args.is_empty() {
            return;
        }
        self.out.push('(');
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                self.out.push_str("; ");
            }
            self.paren(a);
        }
        self.out.push(')');
    }

    fn infix(&mut self, l: &Term<&str>, op: &str, r: &Term<&str>) {
        self.paren(l);
        self.out.push(' ');
        self.out.push_str(op);
        self.out.push(' ');
        self.paren(r);
    }

    /// `L op R` becomes `(R) as $rhs | _dq_vivify(L) | (L) op $rhs`: the
    /// right-hand side sees the input before any path is created, as it
    /// sees it in jq.
    fn assign(&mut self, l: &Term<&str>, op: &str, r: &Term<&str>) {
        self.paren(r);
        self.out.push_str(" as ");
        self.out.push_str(&self.rhs);
        self.out.push_str(" | _dq_vivify");
        self.lhs(l, false);
        self.out.push_str(" | ");
        self.lhs(l, true);
        self.out.push(' ');
        self.out.push_str(op);
        self.out.push(' ');
        self.out.push_str(&self.rhs);
    }

    fn binop(&mut self, l: &Term<&str>, op: &BinaryOp<&str>, r: &Term<&str>) {
        match op {
            BinaryOp::Pipe(None) => self.infix(l, "|", r),
            BinaryOp::Pipe(Some(p)) => {
                self.paren(l);
                self.out.push_str(" as ");
                self.pattern(p);
                self.out.push_str(" | ");
                self.paren(r);
            }
            BinaryOp::Comma => self.infix(l, ",", r),
            BinaryOp::Alt => self.infix(l, "//", r),
            BinaryOp::Or => self.infix(l, "or", r),
            BinaryOp::And => self.infix(l, "and", r),
            BinaryOp::Math(m) => self.infix(l, m.as_str(), r),
            BinaryOp::Cmp(c) => self.infix(l, c.as_str(), r),
            BinaryOp::Assign => self.assign(l, "=", r),
            BinaryOp::UpdateMath(m) => self.assign(l, &format!("{}=", m.as_str()), r),
            BinaryOp::UpdateAlt => self.assign(l, "//=", r),
            // `L |= F` needs no binding: F sees the value at the path.
            BinaryOp::Update => {
                self.out.push_str("_dq_vivify");
                self.lhs(l, false);
                self.out.push_str(" | ");
                self.lhs(l, true);
                self.out.push_str(" |= ");
                self.paren(r);
            }
        }
    }

    fn pattern(&mut self, p: &Pattern<&str>) {
        match p {
            Pattern::Var(x) => self.out.push_str(x),
            Pattern::Arr(items) => {
                self.out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.pattern(item);
                }
                self.out.push(']');
            }
            Pattern::Obj(entries) => {
                self.out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.paren(k);
                    self.out.push_str(": ");
                    self.pattern(v);
                }
                self.out.push('}');
            }
        }
    }

    fn parts(&mut self, parts: &[(Part<Term<&str>>, Opt)]) {
        for (part, opt) in parts {
            self.out.push('[');
            match part {
                Part::Index(i) => self.index(i),
                Part::Range(from, to) => {
                    if let Some(from) = from {
                        self.index(from);
                    }
                    if from.is_some() || to.is_some() {
                        self.out.push(':');
                    }
                    if let Some(to) = to {
                        self.index(to);
                    }
                }
            }
            self.out.push(']');
            if matches!(opt, Opt::Optional) {
                self.out.push('?');
            }
        }
    }

    /// An index or a slice bound, through `_dq_index` on the left-hand
    /// side of an assignment — but a string literal, which it would leave
    /// as it is.
    fn index(&mut self, i: &Term<&str>) {
        if !self.lhs || matches!(i, Term::Str(None, _)) {
            self.paren(i);
            return;
        }
        self.out.push_str("((");
        self.term(i);
        self.out.push_str(") | _dq_index)");
    }

    /// Print a string literal: the parts without escapes as they were, an
    /// escaped character re-escaped where it has to be, an interpolation
    /// rewritten.
    fn str_lit(&mut self, fmt: Option<&str>, parts: &[StrPart<&str, Term<&str>>]) {
        if let Some(fmt) = fmt {
            self.out.push_str(fmt);
            self.out.push(' ');
        }
        self.out.push('"');
        for part in parts {
            match part {
                StrPart::Str(s) => self.out.push_str(s),
                StrPart::Char('"') => self.out.push_str("\\\""),
                StrPart::Char('\\') => self.out.push_str("\\\\"),
                StrPart::Char(c) => self.out.push(*c),
                StrPart::Term(t) => {
                    self.out.push_str("\\(");
                    self.term(t);
                    self.out.push(')');
                }
            }
        }
        self.out.push('"');
    }
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
    fn an_integral_float_index_is_the_integer_it_is() {
        // Each expectation is what jq 1.8 prints for the same program.
        for (filter, input, want) in [
            (".[2.0] = 1", "null", "[null,null,1]"),
            (".[1e3] = 1 | length", "null", "1001"),
            ("[1,2,3] | .[2.0] = 9", "null", "[1,2,9]"),
            (".a[2.0] |= 3", "null", r#"{"a":[null,null,3]}"#),
            (r#"{"a":[1,2]} | .a[-1.0] += 1"#, "null", r#"{"a":[1,3]}"#),
            ("2.0 as $i | .[$i] = 1", "null", "[null,null,1]"),
            ("[1,2,3] | .[1.0:2.0] = [\"x\"]", "null", r#"[1,"x",3]"#),
            ("setpath([2.0]; 1)", "null", "[null,null,1]"),
            ("[1,2,3] | delpaths([[1.0]])", "null", "[1,3]"),
        ] {
            assert_eq!(one(filter, input), want, "{filter} on {input}");
        }
        // Integral, but past the limit.
        for filter in [".[1e19] = 1", ".[1e300] = 1"] {
            let err = run(filter, "null").expect_err(filter);
            assert!(err.contains("Array index too large"), "{filter}: {err}");
        }
    }

    #[test]
    fn an_object_takes_only_a_string_key() {
        // jaq looks any value up as an object key; jq 1.8.2 refuses each of
        // these with the message given.
        for (filter, input, message) in [
            (".[1]", "{}", "Cannot index object with number (1)"),
            (
                r#".[1]"#,
                r#"{"1":2}"#,
                "Cannot index object with number (1)",
            ),
            (".[null]", "{}", "Cannot index object with null (null)"),
            (".[true]", "{}", "Cannot index object with boolean (true)"),
            (".[[1]]", "{}", "Cannot index object with array ([1])"),
            (r#".[1, "a"]"#, "{}", "Cannot index object with number (1)"),
            (
                ".a[.n]",
                r#"{"a":{},"n":1}"#,
                "Cannot index object with number (1)",
            ),
            (".[1] = 1", "{}", "Cannot index object with number (1)"),
            (".[1e300] = 1", "{}", "Cannot index object with number"),
            (".[1] |= 1", "{}", "Cannot index object with number (1)"),
            (
                ".a[1] += 1",
                r#"{"a":{}}"#,
                "Cannot index object with number (1)",
            ),
            (".[1] //= 3", "{}", "Cannot index object with number (1)"),
            ("path(.[1])", "{}", "Cannot index object with number (1)"),
            (
                "setpath([1]; 1)",
                "{}",
                "Cannot index object with number (1)",
            ),
            ("getpath([1])", "{}", "Cannot index object with number (1)"),
            ("del(.[1])", "{}", "Cannot index object with number (1)"),
            (
                "delpaths([[1]])",
                "{}",
                "Cannot delete number field of object",
            ),
            ("pick(.[1])", "{}", "Cannot index object with number (1)"),
            (
                "has(1)",
                "{}",
                "Cannot check whether object has a number key",
            ),
            (
                "has(\"a\")",
                "[1]",
                "Cannot check whether array has a string key",
            ),
            (
                "1 | in({})",
                "null",
                "Cannot check whether object has a number key",
            ),
            ("{(1): 2}", "null", "Cannot use number (1) as object key"),
            (
                "1 as $x | {$x: 2}",
                "null",
                "Cannot use number (1) as object key",
            ),
            (
                "from_entries",
                r#"[{"key":1,"value":2}]"#,
                "Cannot use number (1) as object key",
            ),
            (
                "with_entries(.key |= 1)",
                r#"{"a":1}"#,
                "Cannot use number (1) as object key",
            ),
        ] {
            let err = run(filter, input).expect_err(filter);
            assert!(err.contains(message), "{filter} on {input}: {err}");
        }
    }

    #[test]
    fn an_index_error_names_the_container_and_the_index_as_written() {
        // jq 1.8.2 refuses each of these; past `isize` the index is not
        // saturated, as it is for an array or null.
        for (filter, input, message) in [
            (
                ".[1e300] = 1",
                r#"{"a":1}"#,
                "Cannot index object with number (1e300)",
            ),
            (
                ".[1e19] |= 1",
                r#"{"a":1}"#,
                "Cannot index object with number (1e19)",
            ),
            (
                ".[5] = 1",
                r#"{"a":1}"#,
                "Cannot index object with number (5)",
            ),
            (
                "setpath([1e300]; 1)",
                r#"{"a":1}"#,
                "Cannot index object with number (1e300)",
            ),
            (
                ".[1e300] = 1",
                r#""s""#,
                "Cannot index string with number (1e300)",
            ),
            (
                "setpath([1e300]; 1)",
                r#""s""#,
                "Cannot index string with number (1e300)",
            ),
            (".[0]", r#""s""#, "Cannot index string with number (0)"),
            (".[0] = 1", r#""s""#, "Cannot index string with number (0)"),
            (
                "getpath([0])",
                r#""s""#,
                "Cannot index string with number (0)",
            ),
            ("del(.[0])", r#""s""#, "Cannot index string with number (0)"),
            (
                "has(0)",
                r#""s""#,
                "Cannot check whether string has a number key",
            ),
            (".[0]", "1", "Cannot index number with number (0)"),
            (
                "null as $k | .[$k]",
                "true",
                "Cannot index boolean with null (null)",
            ),
            (".[null]", "[1]", "Cannot index array with null (null)"),
            (".[true]", "[1]", "Cannot index array with boolean (true)"),
        ] {
            let err = run(filter, input).expect_err(filter);
            assert!(err.contains(message), "{filter} on {input}: {err}");
        }
    }

    #[test]
    fn string_keys_and_optional_indices_on_objects_still_work() {
        // Each expectation is what jq 1.8.2 prints for the same program.
        for (filter, input, want) in [
            ("[.[1]?]", "{}", "[]"),
            (".[1]? = 1", "{}", "{}"),
            ("[.[1]?, .a]", "{}", "[null]"),
            (
                "try .[1] catch .",
                "{}",
                r#""Cannot index object with number (1)""#,
            ),
            (".[0]", "[5]", "5"),
            (".[1]", "null", "null"),
            (r#".["a"]"#, "{}", "null"),
            (r#""a" as $k | .[$k] = 1"#, "{}", r#"{"a":1}"#),
            (r#"has("a")"#, r#"{"a":1}"#, "true"),
            ("has(0)", "[1]", "true"),
            (
                "[has(-1), has(0.5), has(1.5), has(nan)]",
                "[1]",
                "[false,true,false,false]",
            ),
            ("has(1)", "null", "false"),
            (r#""a" | in({"a":1})"#, "null", "true"),
            (
                "to_entries | from_entries",
                r#"{"a":1,"b":[2]}"#,
                r#"{"a":1,"b":[2]}"#,
            ),
            ("with_entries(.value += 1)", r#"{"a":1}"#, r#"{"a":2}"#),
            ("from_entries", r#"[{"name":"a","value":1}]"#, r#"{"a":1}"#),
            ("from_entries", r#"[{"Key":"a","Value":3}]"#, r#"{"a":3}"#),
            ("from_entries", r#"[{"key":"a"}]"#, r#"{"a":null}"#),
            (
                "from_entries",
                r#"[{"key":"a","value":null,"Value":2}]"#,
                r#"{"a":null}"#,
            ),
            ("pick(.a[1])", r#"{"a":[1,2]}"#, r#"{"a":[null,2]}"#),
            ("pick(.[1])", "null", "[null,null]"),
            (r#".[{"start": 1, "end": 2}]"#, r#""abc""#, r#""b""#),
            (".[1e300]", "[1]", "null"),
            (".[1e300]", "null", "null"),
            ("setpath([2.0]; 1)", "[0,0,0]", "[0,0,1]"),
            (".[2.0]", "[1,2,3]", "3"),
            ("del(.[1.0])", "[1,2,3]", "[1,3]"),
            (r#"getpath(["a", "b"])"#, "{}", "null"),
            // jq evaluates the last index outermost.
            ("[.[0, 1][0, 1]]", "[[1,2],[3,4]]", "[1,3,2,4]"),
        ] {
            assert_eq!(one(filter, input), want, "{filter} on {input}");
        }
    }

    #[test]
    fn number_indices_read_as_jq_reads_them() {
        // Each expectation is what jq 1.8.2 prints for the same program:
        // a number key is truncated toward zero, one no array position
        // holds (NaN, past isize either way) reads nothing.
        for (filter, input, want) in [
            (
                "[has(-0.5), has(-1), has(0.9), has(1e300), has(-1e300)]",
                "[3]",
                "[true,false,true,false,false]",
            ),
            (".[-1e300]", "[1,2]", "null"),
            ("getpath([1e300])", "[1,2]", "null"),
            ("getpath([-1e300])", "[1,2]", "null"),
            ("getpath([1e300, 2])", "[1,2]", "null"),
            ("[.[-1e300]?]", "[1,2]", "[null]"),
            (".[nan]", "[1,2]", "null"),
            (".[1.7]", "[1,2]", "2"),
            (".[-1.5]", "[1,2]", "2"),
            (".[-0.5]", "[1,2]", "1"),
            ("getpath([-1.5])", "[1,2]", "2"),
            ("del(.[-1e300])", "[1,2]", "[1,2]"),
            (".[1e300]", "[1,2]", "null"),
        ] {
            assert_eq!(one(filter, input), want, "{filter} on {input}");
        }
    }

    #[test]
    fn null_takes_a_string_a_number_or_a_slice_as_key() {
        // jq 1.8.2: `null | .[true]` is "Cannot index null with boolean
        // (true)", where jaq gave null.
        for (filter, want) in [
            (".[true]", "Cannot index null with boolean (true)"),
            (".[null]", "Cannot index null with null (null)"),
            (".[[1]]", "Cannot index null with array ([1])"),
            ("getpath([true])", "Cannot index null with boolean (true)"),
            (".[true] = 1", "Cannot index null with boolean (true)"),
        ] {
            let err = run(filter, "null").expect_err(filter);
            assert!(err.contains(want), "{filter}: {err}");
        }
        for (filter, want) in [
            ("[.[true]?]", "[]"),
            (".[1]", "null"),
            (r#".["a"]"#, "null"),
            (r#".[{"start": 1}]"#, "null"),
            (
                "try .[true] catch .",
                r#""Cannot index null with boolean (true)""#,
            ),
        ] {
            assert_eq!(one(filter, "null"), want, "{filter}");
        }
    }

    #[test]
    fn a_fractional_index_stays_refused() {
        // jq truncates (`.[1.5] = 1` is `[null,1]`); jaq refuses it.
        for (filter, input) in [(".[1.5] = 1", "null"), (".[1.5] = 9", "[1,2,3]")] {
            assert!(run(filter, input).is_err(), "{filter} on {input}");
        }
    }

    #[test]
    fn a_user_variable_is_not_captured_by_the_rewrite() {
        // The left-hand side is evaluated inside the binding of the
        // right-hand side: a variable of the program with that name must
        // still be the program's.
        for (filter, want) in [
            (r#""k" as $__dq_rhs | .[$__dq_rhs] = 1"#, r#"{"k":1}"#),
            (r#""k" as $__dq_rhs | .[$__dq_rhs] |= 1"#, r#"{"k":1}"#),
            (
                r#""k" as $__dq_rhs | "j" as $__dq_rhs1 | .[$__dq_rhs, $__dq_rhs1] = 1"#,
                r#"{"k":1,"j":1}"#,
            ),
        ] {
            assert_eq!(one(filter, "{}"), want, "{filter}");
        }
        let rewritten = rewrite(r#""k" as $__dq_rhs | .a = 1"#).unwrap_or_default();
        assert!(rewritten.contains("(1) as $__dq_rhs1 |"), "{rewritten}");
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
            // Into an array, an integral number past `isize` in any
            // spelling — not "cannot index [1] with 1e300".
            ("[1] | .[1e300] = 1", "null"),
            ("[1] | .[1E300] |= 1", "null"),
            ("[1] | .[1e+300] = 1", "null"),
            ("[1] | .[100000000000000000000] = 1", "null"),
            (".a[.n] = 1", r#"{"a":[1],"n":1e300}"#),
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
        // An index that is not a string literal goes through `_dq_index`,
        // and `_dq_vivify` checks it is no key an object refuses.
        assert_eq!(
            rewrite(".[$i] |= 1").as_deref(),
            Some(
                "_dq_vivify(((.) | _dq_objkey($i) | .[($i | _dq_index)])) \
                 | (.[(($i) | _dq_index)]) |= (1)"
            )
        );
        // `.[k]?` is checked on the update side too, without a native.
        assert_eq!(
            rewrite(".[0]? = 1").as_deref(),
            Some(
                "(1) as $__dq_rhs | _dq_vivify(((.) | _dq_objkey_opt(0) | .[(0 | _dq_index)]?)) \
                 | (((.) | if _dq_badkey(0) then empty else .[(0 | _dq_index)]? end)) = $__dq_rhs"
            )
        );
    }

    #[test]
    fn rewrite_checks_each_index_that_is_not_a_string_literal() {
        assert_eq!(rewrite(".a.b"), None);
        assert_eq!(rewrite(r#".["a"]"#), None);
        assert_eq!(
            rewrite(".[0]").as_deref(),
            Some("((.) | _dq_objkey(0) | .[(0 | _dq_rindex)])")
        );
        // The key is computed once, from the input the indexed term sees;
        // the last index binds outermost, as jq evaluates it.
        assert_eq!(
            rewrite(".a[.k][1]").as_deref(),
            Some(
                "((((.[(\"k\")]) as $__dq_key_0 | (.[(\"a\")]) | _dq_objkey($__dq_key_0) \
                 | .[($__dq_key_0 | _dq_rindex)])) | _dq_objkey(1) | .[(1 | _dq_rindex)])"
            )
        );
        // A variable the program holds is not captured.
        assert_eq!(
            rewrite(".[.k] as $__dq_key | $__dq_key").as_deref(),
            Some(
                "(((.[(\"k\")]) as $__dq_key1_0 | (.) | _dq_objkey($__dq_key1_0) \
                 | .[($__dq_key1_0 | _dq_rindex)])) as $__dq_key | ($__dq_key)"
            )
        );
        assert_eq!(
            rewrite("{(.k): 1, $x, $y: 2}").as_deref(),
            Some("{((.[(\"k\")]) | _dq_key): (1), $x, (($y) | _dq_key): (2)}")
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
