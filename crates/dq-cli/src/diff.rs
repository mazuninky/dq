//! Unified-diff rendering for `dq set --diff` and `dq del --diff` (M2 §11).
//!
//! Wraps the `similar` crate's line-oriented unified-diff formatter behind a
//! single [`render_unified`] entry point. The CLI handlers in
//! `commands::set` / `commands::del` (landing in §9 / §10) call this when the
//! user passes `--diff`; the diff itself is just a presentation layer over
//! the source/modified strings the textual-edit pipeline already produces.
//!
//! The function returns an owned `String` so the caller can hand it to a
//! `&mut dyn Write` in one go without juggling lifetimes against `similar`'s
//! internal buffers.

use similar::TextDiff;

// ANSI escape codes for colored output. `git diff` uses red on removals,
// green on additions, and cyan on hunk headers; we mirror that to keep the
// output familiar.
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_CYAN: &str = "\x1b[36m";

/// Render a unified diff between `source` and `modified`.
///
/// `file_label` is used in the diff header (`--- a/<label>` / `+++ b/<label>`),
/// matching the convention `git diff` and `diff -u` emit.
///
/// `use_color` toggles ANSI escape colors (red `-`, green `+`, cyan `@@`,
/// default for headers and context).
///
/// Returns an empty string when `source == modified` — there is no diff to
/// show, and emitting an empty header would just clutter the CLI output.
/// Otherwise the returned string ends with a trailing newline (matching
/// `git diff` convention).
#[must_use]
pub fn render_unified(source: &str, modified: &str, file_label: &str, use_color: bool) -> String {
    if source == modified {
        return String::new();
    }

    let diff = TextDiff::from_lines(source, modified);
    let header_a = format!("a/{file_label}");
    let header_b = format!("b/{file_label}");

    // `similar` only emits the `--- / +++` header when there is at least one
    // hunk, so an empty raw output here means "the inputs differ in some way
    // similar's line splitter normalises away" (e.g. trailing-newline-only
    // delta). Treat that as "no displayable diff" rather than synthesizing a
    // header for an empty body.
    let raw = diff
        .unified_diff()
        .context_radius(3)
        .header(&header_a, &header_b)
        .to_string();

    if raw.is_empty() {
        return String::new();
    }

    if !use_color {
        return raw;
    }

    colorize(&raw)
}

/// Wrap each line in the appropriate ANSI color sequence.
///
/// Rules (only the leading character of each line drives the choice):
/// - `+` (added line, but NOT the `+++` file header) → green
/// - `-` (removed line, but NOT the `---` file header) → red
/// - `@@` (hunk header) → cyan
/// - everything else (file headers, context lines) → unchanged
fn colorize(raw: &str) -> String {
    // Preserve the trailing newline (if any) by splitting on `\n` so each
    // newline is owned by the line that precedes it. Allocating once with a
    // generous capacity beats appending escape sequences in a tight loop.
    let mut out = String::with_capacity(raw.len() + raw.len() / 8);
    for line in raw.split_inclusive('\n') {
        let color = pick_color(line);
        match color {
            Some(c) => {
                out.push_str(c);
                // Strip the trailing newline from the colored body so the
                // ANSI reset lands BEFORE the newline; otherwise some
                // terminals leave the color set across the line break.
                if let Some(stripped) = line.strip_suffix('\n') {
                    out.push_str(stripped);
                    out.push_str(ANSI_RESET);
                    out.push('\n');
                } else {
                    out.push_str(line);
                    out.push_str(ANSI_RESET);
                }
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Pick the ANSI color for a single diff line based on its prefix.
///
/// `+++` and `---` are file headers (not additions/removals), so they return
/// `None`.
fn pick_color(line: &str) -> Option<&'static str> {
    if line.starts_with("+++") || line.starts_with("---") {
        None
    } else if line.starts_with('+') {
        Some(ANSI_GREEN)
    } else if line.starts_with('-') {
        Some(ANSI_RED)
    } else if line.starts_with("@@") {
        Some(ANSI_CYAN)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_produce_empty_output() {
        let s = "alpha\nbeta\ngamma\n";
        assert_eq!(render_unified(s, s, "doc.yaml", false), "");
        assert_eq!(render_unified(s, s, "doc.yaml", true), "");
    }

    #[test]
    fn single_line_mutation_yields_one_minus_and_one_plus() {
        let src = "alpha\nbeta\ngamma\n";
        let dst = "alpha\nBETA\ngamma\n";
        let out = render_unified(src, dst, "doc.yaml", false);

        // Count lines that BEGIN with `-` / `+` but exclude the file headers
        // (`---` / `+++`). `lines()` gives us per-line slices regardless of
        // the trailing newline.
        let minus_lines = out
            .lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .count();
        let plus_lines = out
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .count();

        assert_eq!(minus_lines, 1, "expected exactly one removal in:\n{out}");
        assert_eq!(plus_lines, 1, "expected exactly one addition in:\n{out}");
        assert!(
            out.contains("--- a/doc.yaml"),
            "expected `a/` header, got:\n{out}"
        );
        assert!(
            out.contains("+++ b/doc.yaml"),
            "expected `b/` header, got:\n{out}"
        );
        assert!(out.ends_with('\n'), "expected trailing newline in:\n{out}");
    }

    #[test]
    fn output_without_color_contains_no_ansi_escapes() {
        let src = "alpha\nbeta\n";
        let dst = "alpha\nBETA\n";
        let out = render_unified(src, dst, "doc.yaml", false);
        assert!(
            !out.contains('\x1b'),
            "uncolored output should not contain ESC, got: {out:?}"
        );
    }

    #[test]
    fn output_with_color_wraps_additions_and_removals() {
        let src = "alpha\nbeta\n";
        let dst = "alpha\nBETA\n";
        let out = render_unified(src, dst, "doc.yaml", true);

        assert!(
            out.contains("\x1b[31m-beta"),
            "expected red on the removal, got:\n{out}"
        );
        assert!(
            out.contains("\x1b[32m+BETA"),
            "expected green on the addition, got:\n{out}"
        );
        assert!(
            out.contains("\x1b[36m@@"),
            "expected cyan on the hunk header, got:\n{out}"
        );
        // File headers stay uncolored — the `---` / `+++` lines must appear
        // verbatim, not wrapped in red/green which would conflict with the
        // additions/removals colors.
        assert!(
            !out.contains("\x1b[31m---"),
            "file `---` header must not be colored as a removal:\n{out}"
        );
        assert!(
            !out.contains("\x1b[32m+++"),
            "file `+++` header must not be colored as an addition:\n{out}"
        );
    }
}
