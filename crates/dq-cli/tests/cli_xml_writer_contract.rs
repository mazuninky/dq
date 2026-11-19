//! The XML writer's contract around the corners `cli_number_read_matrix.rs`
//! does not reach: namespace scope down and across the tree, null content,
//! input encodings without a byte order mark and with malformed bytes, a
//! character the declared encoding cannot hold in every place it can sit,
//! encoding labels by their IANA aliases, and what `-i` and a bulk run do
//! with a file the writer refuses.
//!
//! Every run goes through `dq --no-color -q`; stdout is compared as bytes,
//! since the output is encoded in the declared encoding.

use assert_cmd::Command;
use tempfile::TempDir;

/// Build a `dq` command with a clean environment (same discipline as
/// `cli_smoke.rs::dq`).
fn dq() -> Command {
    let mut cmd = Command::cargo_bin("dq").expect("dq binary built");
    cmd.env_clear();
    if let Ok(p) = std::env::var("PATH") {
        cmd.env("PATH", p);
    }
    cmd.env("HOME", "/tmp");
    cmd
}

/// A temp dir holding `files`, given as bytes.
fn dir_with(files: &[(&str, &[u8])]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).expect("write fixture");
    }
    dir
}

/// `(exit code, stdout, stderr)` of `dq --no-color -q <args>` in `dir`.
fn run(dir: &TempDir, args: &[&str]) -> (i32, Vec<u8>, String) {
    let out = dq()
        .current_dir(dir.path())
        .args(["--no-color", "-q"])
        .args(args)
        .output()
        .expect("dq runs");
    (
        out.status.code().expect("exit code"),
        out.stdout,
        String::from_utf8(out.stderr).expect("UTF-8 stderr"),
    )
}

/// The `(exit code, stdout, stderr)` a run is expected to end with.
type Outcome = (i32, Vec<u8>, String);

/// `(0, stdout, "")`.
fn ok(stdout: impl Into<Vec<u8>>) -> Outcome {
    (0, stdout.into(), String::new())
}

/// The XML writer's refusal: exit 1 and `message` on stderr.
fn refused(message: &str) -> Outcome {
    (
        1,
        Vec::new(),
        format!("xml format error: XML write: {message}\n"),
    )
}

/// A reader's refusal of `file`: exit 3 and `message` on stderr.
fn unreadable(file: &str, message: &str) -> Outcome {
    (3, Vec::new(), format!("parse error in {file}: {message}\n"))
}

/// Run every `(args, outcome)` case in `dir` and fail once, listing every
/// case whose outcome differs.
fn assert_cases(dir: &TempDir, cases: &[(Vec<&str>, Outcome)]) {
    let mut wrong = Vec::new();
    for (args, expected) in cases {
        let actual = run(dir, args);
        if actual != *expected {
            wrong.push(format!(
                "{args:?}\n  expected {} {:?} {:?}\n  actual   {} {:?} {:?}",
                expected.0,
                String::from_utf8_lossy(&expected.1),
                expected.2,
                actual.0,
                String::from_utf8_lossy(&actual.1),
                actual.2,
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} case(s) differ:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// `convert <file> -F <to>`.
fn convert<'a>(file: &'a str, to: &'a str) -> Vec<&'a str> {
    vec!["convert", file, "-F", to]
}

/// `text` as UTF-16, little- or big-endian, with no byte order mark.
fn utf16(text: &str, big_endian: bool) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(|unit| {
            if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            }
        })
        .collect()
}

/// `bytes` behind `bom`.
fn with_bom(bom: &[u8], bytes: Vec<u8>) -> Vec<u8> {
    let mut out = bom.to_vec();
    out.extend(bytes);
    out
}

/// A declaration on an element reaches its children and their children;
/// it does not reach the element's parent or siblings. `xml` binds its own
/// URI, a default `xmlns` may be redeclared below, and a prefix may be
/// redeclared on a child.
#[test]
fn a_namespace_declaration_reaches_descendants_but_not_ancestors() {
    let dir = dir_with(&[
        (
            "grandchild.yaml",
            b"r:\n- \"@attrs\": {\"xmlns:p\": u}\n  k:\n  - g:\n    - \"p:z\": [{\"@attrs\": {\"p:a\": 1}}]\n",
        ),
        (
            "same.yaml",
            b"\"p:r\":\n- \"@attrs\": {\"p:a\": 1, \"xmlns:p\": u}\n",
        ),
        (
            "xml_bound.yaml",
            b"r:\n- \"@attrs\": {\"xmlns:xml\": \"http://www.w3.org/XML/1998/namespace\", \"xml:lang\": en}\n",
        ),
        (
            "default.yaml",
            b"r:\n- \"@attrs\": {xmlns: \"urn:a\"}\n  k: [{\"@attrs\": {xmlns: \"urn:b\"}}]\n",
        ),
        (
            "redeclared.yaml",
            b"r:\n- \"@attrs\": {\"xmlns:p\": u}\n  k: [{\"@attrs\": {\"xmlns:p\": v}, \"p:x\": [{}]}]\n  \"p:y\": [{}]\n",
        ),
        (
            "parent_attr.yaml",
            b"r:\n- \"@attrs\": {\"p:a\": 1}\n  k: [{\"@attrs\": {\"xmlns:p\": u}}]\n",
        ),
        (
            "parent_element.yaml",
            b"\"p:r\":\n- k: [{\"@attrs\": {\"xmlns:p\": u}}]\n",
        ),
    ]);
    assert_cases(
        &dir,
        &[
            (
                convert("grandchild.yaml", "xml"),
                ok(
                    "<r xmlns:p=\"u\">\n  <k>\n    <g>\n      <p:z p:a=\"1\"/>\n    </g>\n  </k>\n</r>",
                ),
            ),
            (
                convert("same.yaml", "xml"),
                ok("<p:r p:a=\"1\" xmlns:p=\"u\"/>"),
            ),
            (
                convert("xml_bound.yaml", "xml"),
                ok("<r xmlns:xml=\"http://www.w3.org/XML/1998/namespace\" xml:lang=\"en\"/>"),
            ),
            (
                convert("default.yaml", "xml"),
                ok("<r xmlns=\"urn:a\">\n  <k xmlns=\"urn:b\"/>\n</r>"),
            ),
            (
                convert("redeclared.yaml", "xml"),
                ok("<r xmlns:p=\"u\">\n  <k xmlns:p=\"v\">\n    <p:x/>\n  </k>\n  <p:y/>\n</r>"),
            ),
            (
                convert("parent_attr.yaml", "xml"),
                refused(
                    "attribute 'p:a' on <r> uses the prefix 'p', which no 'xmlns:p' declares \
                     in scope",
                ),
            ),
            (
                convert("parent_element.yaml", "xml"),
                refused("element <p:r> uses the prefix 'p', which no 'xmlns:p' declares in scope"),
            ),
        ],
    );
}

/// A null is no content: a null `#xml`, `#comments`, `#pi`, `#text`,
/// `#cdata`, comment item or child is left out, and an `#xml` whose fields
/// are all null still writes the declaration with its default version.
#[test]
fn null_content_is_left_out() {
    let dir = dir_with(&[
        ("decl.yaml", b"\"#xml\": null\nr:\n- {}\n"),
        (
            "top.yaml",
            b"\"#comments\": null\n\"#pi\": null\n\"@attrs\": null\n\"#text\": null\n\
              \"#cdata\": null\nr:\n- {}\n",
        ),
        (
            "element.yaml",
            b"r:\n- \"#text\": null\n  \"#cdata\": null\n  \"#comments\": [null]\n",
        ),
        (
            "fields.yaml",
            b"\"#xml\": {version: null, encoding: null, standalone: null}\nr:\n- {}\n",
        ),
        ("child.yaml", b"r:\n- k: null\n"),
        ("child_item.yaml", b"r:\n- k: [null]\n"),
    ]);
    assert_cases(
        &dir,
        &[
            (convert("decl.yaml", "xml"), ok("<r/>")),
            (convert("top.yaml", "xml"), ok("<r/>")),
            (convert("element.yaml", "xml"), ok("<r/>")),
            (
                convert("fields.yaml", "xml"),
                ok("<?xml version=\"1.0\"?><r/>"),
            ),
            (convert("child.yaml", "xml"), ok("<r/>")),
            (convert("child_item.yaml", "xml"), ok("<r>\n  <k/>\n</r>")),
        ],
    );
}

/// UTF-16 with no byte order mark reads by its declaration, and UTF-16BE
/// declared so writes back byte for byte; a big-endian BOM alone is enough
/// and is kept as `#xml.bom`. A UTF-8 BOM reads as UTF-8. Before, each of
/// the UTF-16 files was a parse error.
#[test]
fn utf16_reads_by_its_declaration_without_a_bom() {
    let le = utf16(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><r>\u{e9}</r>",
        false,
    );
    let be = utf16(
        "<?xml version=\"1.0\" encoding=\"UTF-16BE\"?><r>\u{e9}</r>",
        true,
    );
    let be_bom = with_bom(&[0xFE, 0xFF], utf16("<r>\u{e9}</r>", true));
    let utf8_bom = with_bom(&[0xEF, 0xBB, 0xBF], "<r>\u{e9}</r>".as_bytes().to_vec());
    let dir = dir_with(&[
        ("le.xml", &le),
        ("be.xml", &be),
        ("be_bom.xml", &be_bom),
        ("utf8_bom.xml", &utf8_bom),
    ]);
    let json = |decl: &str| {
        format!("{{\n{decl}  \"r\": [\n    {{\n      \"#text\": \"\u{e9}\"\n    }}\n  ]\n}}\n")
    };
    assert_cases(
        &dir,
        &[
            (
                convert("le.xml", "json"),
                ok(json(
                    "  \"#xml\": {\n    \"version\": \"1.0\",\n    \"encoding\": \"UTF-16\"\n  },\n",
                )),
            ),
            (
                convert("be.xml", "json"),
                ok(json(
                    "  \"#xml\": {\n    \"version\": \"1.0\",\n    \"encoding\": \"UTF-16BE\"\n  },\n",
                )),
            ),
            (convert("be.xml", "xml"), ok(be.clone())),
            (
                convert("be_bom.xml", "json"),
                ok(json("  \"#xml\": {\n    \"bom\": \"UTF-16BE\"\n  },\n")),
            ),
            (convert("be_bom.xml", "xml"), ok(be_bom.clone())),
            (convert("utf8_bom.xml", "json"), ok(json(""))),
        ],
    );
}

/// Bytes the declared (or BOM-given) encoding cannot decode are a parse
/// error, exit 3, naming the encoding: an odd UTF-16 byte count, a lone
/// surrogate, a Shift_JIS lead byte with no trail. UTF-16 with neither a
/// BOM nor a declaration reads as UTF-8 and fails there.
#[test]
fn malformed_bytes_in_the_input_encoding_are_a_parse_error() {
    let le_bom = |text: &str, tail: &[u8]| {
        let mut bytes = with_bom(&[0xFF, 0xFE], utf16(text, false));
        bytes.extend(tail);
        bytes
    };
    let odd = le_bom("<r>\u{e9}</r>", &[0x00]);
    let mut lone = le_bom("<r>", &[0x00, 0xD8]);
    lone.extend(utf16("</r>", false));
    let dir = dir_with(&[
        ("odd.xml", &odd),
        ("lone.xml", &lone),
        (
            "sjis.xml",
            b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?><r>\x81</r>",
        ),
        ("bare.xml", &utf16("<r>\u{e9}</r>", false)),
    ]);
    assert_cases(
        &dir,
        &[
            (
                convert("odd.xml", "json"),
                unreadable("odd.xml", "invalid UTF-16LE in XML input"),
            ),
            (
                convert("lone.xml", "json"),
                unreadable("lone.xml", "invalid UTF-16LE in XML input"),
            ),
            (
                convert("sjis.xml", "json"),
                unreadable("sjis.xml", "invalid Shift_JIS in XML input"),
            ),
            (
                convert("bare.xml", "json"),
                unreadable(
                    "bare.xml",
                    "invalid UTF-8 in XML input: invalid utf-8 sequence of 1 bytes from index 6",
                ),
            ),
        ],
    );
}

/// A character the declared encoding cannot hold is refused wherever it
/// sits — element name, attribute name or value, text, CDATA, comment, PI
/// target or data, at any depth or at the top level — and the message names
/// the place. Before, the writer emitted UTF-8 under the US-ASCII label.
#[test]
fn a_character_the_encoding_cannot_hold_is_refused_in_every_place() {
    let place = |body: &str| format!("\"#xml\": {{encoding: US-ASCII}}\n{body}");
    let files = [
        ("root.yaml", place("\"é\":\n- {}\n")),
        ("child.yaml", place("r:\n- \"ké\": [{}]\n")),
        ("attr_name.yaml", place("r:\n- \"@attrs\": {\"é\": 1}\n")),
        ("attr_value.yaml", place("r:\n- \"@attrs\": {a: é}\n")),
        ("text.yaml", place("r:\n- \"#text\": é\n")),
        ("cdata.yaml", place("r:\n- \"#cdata\": [é]\n")),
        ("comment.yaml", place("r:\n- \"#comments\": [é]\n")),
        ("pi_data.yaml", place("r:\n- \"#pi\": [\"t é\"]\n")),
        ("pi_target.yaml", place("r:\n- \"#pi\": [\"té x\"]\n")),
        ("top_pi.yaml", place("\"#pi\": [\"t é\"]\nr:\n- {}\n")),
        ("deep.yaml", place("r:\n- k:\n  - g: [{\"#text\": é}]\n")),
        (
            "first.yaml",
            place("r:\n- \"#text\": \"a\\u00e9\"\n  k: [{\"#text\": \"\\U0001F600\"}]\n"),
        ),
    ];
    let dir = dir_with(&files.each_ref().map(|(name, src)| (*name, src.as_bytes())));
    let cannot = |place: &str| {
        refused(&format!(
            "'#xml' declares encoding \"US-ASCII\", which cannot hold U+00E9 {place}"
        ))
    };
    assert_cases(
        &dir,
        &[
            (convert("root.yaml", "xml"), cannot("in <é>")),
            (convert("child.yaml", "xml"), cannot("in <ké>")),
            (convert("attr_name.yaml", "xml"), cannot("in <r>")),
            (convert("attr_value.yaml", "xml"), cannot("in <r>")),
            (convert("text.yaml", "xml"), cannot("in <r>")),
            (convert("cdata.yaml", "xml"), cannot("in <r>")),
            (convert("comment.yaml", "xml"), cannot("in <r>")),
            (convert("pi_data.yaml", "xml"), cannot("in <r>")),
            (convert("pi_target.yaml", "xml"), cannot("in <r>")),
            (convert("top_pi.yaml", "xml"), cannot("at the top level")),
            (convert("deep.yaml", "xml"), cannot("in <g>")),
            (convert("first.yaml", "xml"), cannot("in <r>")),
        ],
    );
}

/// An encoding label is read by its IANA aliases: every ISO-8859-1 alias
/// maps code point to byte (U+0080 is 0x80), every US-ASCII alias is strict
/// 7-bit both ways, the UTF-8 aliases write UTF-8, `UTF-16LE` / `UTF-16BE`
/// write no BOM and the generic UTF-16 labels a little-endian one. An
/// encoding dq can read but not write, and a label with spaces around it,
/// are refused.
#[test]
fn encoding_labels_follow_their_iana_aliases() {
    let tree = |label: &str| {
        format!("\"#xml\": {{encoding: \"{label}\"}}\nr:\n- \"#text\": \"\\x80\\u00e9\"\n")
    };
    let latin = [
        "latin1",
        "l1",
        "ISO_8859-1",
        "iso-ir-100",
        "cp819",
        "IBM819",
        "csISOLatin1",
        "iso88591",
    ];
    let ascii = [
        "ascii",
        "ANSI_X3.4-1968",
        "iso-ir-6",
        "us",
        "IBM367",
        "cp367",
        "csASCII",
        "ISO646-US",
    ];
    let utf8 = ["UTF8", "unicode-1-1-utf-8"];
    let utf16_no_bom = [("utf-16le", false), ("UTF-16BE", true), ("utf-16be", true)];
    let utf16_bom = ["utf-16", "ucs-2", "unicode"];
    let unwritable = ["replacement", "ISO-2022-KR", "hz-gb-2312"];
    let spaced = [" UTF-8", "UTF-8 "];
    let labels = latin
        .iter()
        .chain(&ascii)
        .chain(&utf8)
        .chain(utf16_no_bom.iter().map(|(label, _)| label))
        .chain(&utf16_bom)
        .chain(&unwritable)
        .chain(&spaced);
    let mut files: Vec<(String, Vec<u8>)> = labels
        .enumerate()
        .map(|(i, label)| (format!("{i}.yaml"), tree(label).into_bytes()))
        .collect();
    files.push((
        "cp819.xml".to_owned(),
        b"<?xml version='1.0' encoding='cp819'?><r>\x80</r>".to_vec(),
    ));
    files.push((
        "ansi.xml".to_owned(),
        b"<?xml version='1.0' encoding='ANSI_X3.4-1968'?><r>\x80</r>".to_vec(),
    ));
    let dir = dir_with(
        &files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
    );
    let declaration = |label: &str| format!("<?xml version=\"1.0\" encoding=\"{label}\"?>");
    let mut expected = Vec::new();
    for label in latin {
        let mut bytes = declaration(label).into_bytes();
        bytes.extend(b"<r>\x80\xe9</r>");
        expected.push(ok(bytes));
    }
    for label in ascii {
        expected.push(refused(&format!(
            "'#xml' declares encoding \"{label}\", which cannot hold U+0080 in <r>"
        )));
    }
    for label in utf8 {
        expected.push(ok(format!("{}<r>\u{80}\u{e9}</r>", declaration(label))));
    }
    for (label, big_endian) in utf16_no_bom {
        expected.push(ok(utf16(
            &format!("{}<r>\u{80}\u{e9}</r>", declaration(label)),
            big_endian,
        )));
    }
    for label in utf16_bom {
        expected.push(ok(with_bom(
            &[0xFF, 0xFE],
            utf16(&format!("{}<r>\u{80}\u{e9}</r>", declaration(label)), false),
        )));
    }
    for label in unwritable {
        expected.push(refused(&format!(
            "'#xml' declares encoding \"{label}\", which dq cannot write"
        )));
    }
    for label in spaced {
        expected.push(refused(&format!(
            "'#xml' field 'encoding' is not an encoding name (got {label:?})"
        )));
    }
    let mut cases: Vec<(Vec<&str>, Outcome)> = files
        .iter()
        .zip(expected)
        .map(|((name, _), outcome)| (convert(name, "xml"), outcome))
        .collect();
    cases.push((
        convert("cp819.xml", "json"),
        ok(
            "{\n  \"#xml\": {\n    \"version\": \"1.0\",\n    \"encoding\": \"cp819\"\n  },\n  \
             \"r\": [\n    {\n      \"#text\": \"\u{80}\"\n    }\n  ]\n}\n",
        ),
    ));
    cases.push((
        convert("ansi.xml", "json"),
        unreadable(
            "ansi.xml",
            "invalid US-ASCII in XML input: byte 0x80 at index 50",
        ),
    ));
    assert_cases(&dir, &cases);
}

const ASCII_XML: &[u8] = b"<?xml version=\"1.0\" encoding=\"US-ASCII\"?><r>a</r>";
const PLAIN_XML: &[u8] = b"<r>b</r>";
const LATIN_XML: &[u8] = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r>c</r>";

/// `a.xml` (US-ASCII), `b.xml` (UTF-8) and `c.xml` (ISO-8859-1).
fn three_xml_files() -> TempDir {
    dir_with(&[
        ("a.xml", ASCII_XML),
        ("b.xml", PLAIN_XML),
        ("c.xml", LATIN_XML),
    ])
}

/// The bytes of `name` in `dir`.
fn read(dir: &TempDir, name: &str) -> Vec<u8> {
    std::fs::read(dir.path().join(name)).expect("read back")
}

const SET_E_ACUTE: &str = ".r[0].\"#text\" = \"é\"";
const ASCII_REFUSAL: &str = "xml format error: XML write: '#xml' declares encoding \"US-ASCII\", which cannot hold \
     U+00E9 in <r>";

/// `-i` on a file the writer refuses leaves it as it was and exits 1; in a
/// bulk run with `--continue-on-error` that file counts as Failed (exit 7)
/// while the others are written, each in its own encoding. Without
/// `--continue-on-error` the run stops at the refusal. Before, the writer
/// put `é` under the US-ASCII label and every file counted as Modified.
#[test]
fn a_refused_xml_write_leaves_the_file_untouched() {
    let dir = three_xml_files();
    assert_eq!(
        run(&dir, &["-i", "set", "a.xml", "--jq", SET_E_ACUTE]),
        (1, Vec::new(), format!("{ASCII_REFUSAL}\n"))
    );
    assert_eq!(read(&dir, "a.xml"), ASCII_XML);

    assert_eq!(
        run(&dir, &["-i", "set", "*.xml", "--jq", SET_E_ACUTE]),
        (1, Vec::new(), format!("{ASCII_REFUSAL}\n"))
    );
    assert_eq!(
        [
            read(&dir, "a.xml"),
            read(&dir, "b.xml"),
            read(&dir, "c.xml")
        ],
        [ASCII_XML, PLAIN_XML, LATIN_XML]
    );

    let (code, stdout, stderr) = run(
        &dir,
        &[
            "-i",
            "--continue-on-error",
            "set",
            "*.xml",
            "--jq",
            SET_E_ACUTE,
        ],
    );
    assert_eq!(
        (code, String::from_utf8_lossy(&stdout).into_owned(), stderr),
        (
            7,
            format!("Modified: 2, Skipped: 0, Failed: 1\n  - ./a.xml: {ASCII_REFUSAL}\n"),
            "1 file(s) failed in bulk run\n".to_owned()
        )
    );
    assert_eq!(
        [
            read(&dir, "a.xml"),
            read(&dir, "b.xml"),
            read(&dir, "c.xml")
        ],
        [
            ASCII_XML.to_vec(),
            "<r>é</r>".as_bytes().to_vec(),
            b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><r>\xe9</r>".to_vec()
        ]
    );
}

/// Every file of a bulk run refused: nothing is written, all count as
/// Failed, exit 7. `fix -i` takes the same road as `set -i`.
#[test]
fn a_bulk_run_counts_each_refused_xml_write_as_failed() {
    let dir = three_xml_files();
    let comment = "xml format error: XML write: a comment in '#comments' of <r> contains '--', \
                   which XML does not allow in a comment";
    let (code, stdout, stderr) = run(
        &dir,
        &[
            "-i",
            "--continue-on-error",
            "set",
            "*.xml",
            "--jq",
            ".r[0].\"#comments\" = [\"a--b\"]",
        ],
    );
    assert_eq!(
        (code, String::from_utf8_lossy(&stdout).into_owned(), stderr),
        (
            7,
            format!(
                "Modified: 0, Skipped: 0, Failed: 3\n  - ./a.xml: {comment}\n  \
                 - ./b.xml: {comment}\n  - ./c.xml: {comment}\n"
            ),
            "3 file(s) failed in bulk run\n".to_owned()
        )
    );
    assert_eq!(
        [
            read(&dir, "a.xml"),
            read(&dir, "b.xml"),
            read(&dir, "c.xml")
        ],
        [ASCII_XML, PLAIN_XML, LATIN_XML]
    );

    let rule = "id: t.fix\ndescription: x\nseverity: warn\nmatch: { format: xml }\n\
                check:\n  jq: 'select(.r[0].\"#text\" != \"é\")'\n  message: m\n\
                fix:\n  jq: '.r[0].\"#text\" = \"é\"'\n";
    std::fs::create_dir_all(dir.path().join(".dq/rules")).expect("mkdir rules");
    std::fs::write(dir.path().join(".dq/rules/fix.yml"), rule).expect("write rule");
    let (code, stdout, stderr) = run(
        &dir,
        &["-i", "--continue-on-error", "fix", "a.xml", "b.xml"],
    );
    assert_eq!(
        (code, String::from_utf8_lossy(&stdout).into_owned(), stderr),
        (
            7,
            format!("Modified: 1, Skipped: 0, Failed: 1\n  - a.xml: {ASCII_REFUSAL}\n"),
            "1 file(s) failed in bulk run\n".to_owned()
        )
    );
    assert_eq!(
        [read(&dir, "a.xml"), read(&dir, "b.xml")],
        [ASCII_XML.to_vec(), "<r>é</r>".as_bytes().to_vec()]
    );
}
