//! Shared by the tests that compare the whole stderr of a parse error.

/// `stderr` without the source line and the caret below every
/// `  --> FILE:LINE:COL`, so a test compares the lines it is about as they
/// are. The block must be there, and have its shape: the gutter as wide as
/// LINE, the source row, the caret row with nothing but `^` and `~`, each
/// ended by a line break — and the caret in column COL when the row is the source as it is (ASCII, no
/// escape, no tab).
pub fn without_source_block(stderr: &str) -> String {
    let mut kept = String::new();
    let mut lines = stderr.split_inclusive('\n');
    while let Some(place) = lines.next() {
        kept.push_str(place);
        let Some(at) = place.strip_prefix("  --> ") else {
            continue;
        };
        let mut place = at.trim_end_matches('\n').rsplitn(3, ':');
        let col: usize = place.next().and_then(|c| c.parse().ok()).expect("COL");
        let line = place.next().expect("LINE");
        let mut row = |gutter: &str| {
            let row = lines.next().unwrap_or_default();
            let row = row
                .strip_suffix('\n')
                .unwrap_or_else(|| panic!("no line break after {row:?} in {stderr:?}"));
            row.strip_prefix(gutter)
                .unwrap_or_else(|| panic!("no {gutter:?} row in {stderr:?}"))
                .to_owned()
        };
        let empty = format!("{}|", " ".repeat(line.len() + 2));
        assert_eq!(row(&empty), "", "{stderr:?}");
        let source = row(&format!(" {line} |"));
        assert!(source.is_empty() || source.starts_with(' '), "{stderr:?}");
        let caret = row(&format!("{empty} "));
        let spaces = caret.find('^').expect("a caret");
        assert!(
            caret[..spaces].bytes().all(|b| b == b' ')
                && caret[spaces + 1..].bytes().all(|b| b == b'~'),
            "{stderr:?}"
        );
        let source = source.get(1..).unwrap_or_default();
        if source.is_ascii() && !source.contains('\\') && !source.contains("    ") {
            assert_eq!(spaces + 1, col, "{stderr:?}");
            assert!(caret.len() <= source.len().max(spaces + 1), "{stderr:?}");
        }
    }
    kept
}
