//! jq's rule that an object key is a string, on top of jaq.
//!
//! jaq looks any value up as an object key: `{} | .[1]` is `null`,
//! `{} | .[1] = 1` writes the key `1`, `{(1): 2}` builds one and `has(1)`
//! is `false`. jq refuses each of them ("Cannot index object with number
//! (1)", "Cannot use number (1) as object key", "Cannot check whether
//! object has a number key").
//!
//! The natives here check the key where jaq would take it:
//!
//! - `_dq_objkey($k)` passes its input through, or fails when the input is
//!   an object and `$k` is not a string. It works in path and update mode
//!   too, so the rewrite in [`super::paths`] puts it before every index
//!   that is not a string literal: `t[e]` becomes
//!   `(e) as $k | t | _dq_objkey($k) | .[$k]`.
//! - `_dq_objkey_opt($k)` is the same for `t[e]?`: on a refused key it
//!   yields nothing, and an update leaves its input as it is, as jq's
//!   `{} | .[1]? = 1` does.
//! - `_dq_badkey($k)` is whether `_dq_objkey($k)` would fail: the check
//!   on the side of an assignment the update runs on, where a native in
//!   path or update mode would copy the document (see
//!   [`super::paths`]).
//! - `_dq_key` passes a string through and fails on anything else: a
//!   computed key in an object construction.
//! - `_dq_has($k)` is jq's `has`.

use jaq_core::box_iter::box_once;
use jaq_core::data::JustLut;
use jaq_core::native::{Fun, v};
use jaq_core::{Error, Exn, Native};
use jaq_json::Val;
use jaq_std::ValT as _;

type D = JustLut<Val>;

/// The natives this module defines.
pub(super) fn funs() -> impl Iterator<Item = Fun<D>> {
    let objkey = Native::new(|mut cv| {
        let k = cv.0.pop_var();
        box_once(check_index(&cv.1, &k).map(|()| cv.1).map_err(Exn::from))
    })
    .with_paths(|mut cv| {
        let k = cv.0.pop_var();
        box_once(check_index(&cv.1.0, &k).map(|()| cv.1).map_err(Exn::from))
    })
    .with_update(|mut cv, f| {
        let k = cv.0.pop_var();
        match check_index(&cv.1, &k) {
            Ok(()) => f(cv.1),
            Err(e) => box_once(Err(Exn::from(e))),
        }
    });
    let objkey_opt = Native::new(|mut cv| {
        let k = cv.0.pop_var();
        match check_index(&cv.1, &k) {
            Ok(()) => box_once(Ok(cv.1)),
            Err(_) => Box::new(core::iter::empty()),
        }
    })
    .with_paths(|mut cv| {
        let k = cv.0.pop_var();
        match check_index(&cv.1.0, &k) {
            Ok(()) => box_once(Ok(cv.1)),
            Err(_) => Box::new(core::iter::empty()),
        }
    })
    .with_update(|mut cv, f| {
        let k = cv.0.pop_var();
        match check_index(&cv.1, &k) {
            Ok(()) => f(cv.1),
            Err(_) => box_once(Ok(cv.1)),
        }
    });
    let key = Native::new(|cv| {
        box_once(if is_str(&cv.1) {
            Ok(cv.1)
        } else {
            let msg = format!("Cannot use {} ({}) as object key", type_name(&cv.1), cv.1);
            Err(Exn::from(Error::str(msg)))
        })
    });
    let badkey = Native::new(|mut cv| {
        let k = cv.0.pop_var();
        box_once(Ok(Val::from(check_index(&cv.1, &k).is_err())))
    });
    let has = Native::new(|mut cv| {
        let k = cv.0.pop_var();
        box_once(has(&cv.1, &k).map(Val::from).map_err(Exn::from))
    });
    [
        ("_dq_objkey", v(1), objkey),
        ("_dq_objkey_opt", v(1), objkey_opt),
        ("_dq_key", v(0), key),
        ("_dq_badkey", v(1), badkey),
        ("_dq_has", v(1), has),
    ]
    .into_iter()
}

/// jq's type name of `v`, as its error messages spell it.
pub(super) fn type_name(v: &Val) -> &'static str {
    match v {
        Val::Null => "null",
        Val::Bool(_) => "boolean",
        Val::Num(_) => "number",
        Val::TStr(_) | Val::BStr(_) => "string",
        Val::Arr(_) => "array",
        Val::Obj(_) => "object",
    }
}

pub(super) fn is_str(v: &Val) -> bool {
    matches!(v, Val::TStr(_) | Val::BStr(_))
}

/// Whether `container` may be indexed with `key`: anything but an object
/// takes any key here (jaq reports what it refuses itself); an object
/// takes a string only.
fn check_index(container: &Val, key: &Val) -> Result<(), Error<Val>> {
    if matches!(container, Val::Obj(_)) && !is_str(key) {
        return Err(index_error(key));
    }
    Ok(())
}

/// jq's message for an object indexed with the non-string `key`.
pub(super) fn index_error(key: &Val) -> Error<Val> {
    Error::str(format_args!(
        "Cannot index object with {} ({key})",
        type_name(key)
    ))
}

/// jq's `has`: an object has string keys, an array number keys in
/// `0 <= k < length` (a fraction included, NaN not), null has nothing;
/// any other pairing is an error.
fn has(v: &Val, k: &Val) -> Result<bool, Error<Val>> {
    match (v, k) {
        (Val::Null, _) => Ok(false),
        (Val::Obj(map), _) if is_str(k) => Ok(map.contains_key(k)),
        (Val::Arr(items), Val::Num(_)) => {
            let f = k.as_f64().unwrap_or(f64::NAN);
            Ok(f >= 0.0 && f < items.len() as f64)
        }
        _ => Err(Error::str(format_args!(
            "Cannot check whether {} has a {} key",
            type_name(v),
            type_name(k)
        ))),
    }
}
