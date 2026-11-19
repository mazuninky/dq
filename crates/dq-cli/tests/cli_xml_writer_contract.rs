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
/// and is kept as `#xml.bom`. A UTF-8 BOM reads as UTF-8 and is kept as
/// `#xml.bom` too. Before, each of
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
                    "  \"#xml\": {\n    \"version\": \"1.0\",\n    \"encoding\": \"UTF-16\",\n    \
                     \"detected\": \"UTF-16LE\"\n  },\n",
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
            (
                convert("utf8_bom.xml", "json"),
                ok(json("  \"#xml\": {\n    \"bom\": \"UTF-8\"\n  },\n")),
            ),
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
        run(
            &dir,
            &["-i", "set", "c.xml", "--jq", ".r[0].\"#text\" = \"ж\""]
        ),
        (
            1,
            Vec::new(),
            "xml format error: XML write: '#xml' declares encoding \"ISO-8859-1\", which cannot \
             hold U+0436 in <r>\n"
                .to_owned()
        )
    );
    assert_eq!(read(&dir, "c.xml"), LATIN_XML);

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

/// Text that is all whitespace reads from character references (`&#9;`)
/// and is written back as references, every character of it: written as
/// it is, the reader took it for indentation and dropped it (`<d>\t</d>`
/// read back as `{"d":[{}]}`).
#[test]
fn whitespace_only_text_is_written_as_character_references() {
    let dir = dir_with(&[
        ("tab.xml", b"<d>&#9;</d>"),
        ("lf.xml", b"<d>&#10;</d>"),
        ("nel.xml", b"<d>&#x85;</d>"),
        ("cdata.xml", b"<a><![CDATA[]]>&#10;<![CDATA[x]]></a>"),
        ("mixed.xml", b"<d>&#x2028;&#32;&#13;</d>"),
    ]);
    let cases = [
        ("tab.xml", "<d>&#9;</d>"),
        ("lf.xml", "<d>&#10;</d>"),
        ("nel.xml", "<d>&#x85;</d>"),
        ("cdata.xml", "<a>&#10;<![CDATA[]]><![CDATA[x]]></a>"),
        ("mixed.xml", "<d>&#x2028;&#32;&#13;</d>"),
    ];
    assert_cases(
        &dir,
        &cases
            .iter()
            .map(|(file, written)| (convert(file, "xml"), ok(*written)))
            .collect::<Vec<_>>(),
    );
    // What is written reads back as the tree it was written from.
    for (file, written) in cases {
        let back = format!("back-{file}");
        std::fs::write(dir.path().join(&back), written).expect("write");
        let (_, first, _) = run(&dir, &convert(file, "json"));
        let (code, second, stderr) = run(&dir, &convert(&back, "json"));
        assert_eq!(
            (code, String::from_utf8_lossy(&second), stderr.as_str()),
            (0, String::from_utf8_lossy(&first), ""),
            "{file}"
        );
    }
}

/// UTF-16 with no byte order mark, told by its leading `<?`, is written
/// back in its own byte order with no BOM, the declaration or none, and
/// `#xml.detected` keeps the order — unless the declaration names it. A
/// declaration the UTF-16 contradicts is a parse error, exit 3, as it is
/// after a BOM. Before, the files without an `encoding` came back as UTF-8,
/// the `UTF-16` one as little-endian with a BOM, and every contradiction
/// read.
#[test]
fn utf16_without_a_bom_keeps_its_byte_order_and_refuses_a_contradiction() {
    let decl = |encoding: &str| format!("<?xml version=\"1.0\"{encoding}?><r>\u{e9}</r>");
    let le = utf16(&decl(""), false);
    let be = utf16(&decl(""), true);
    let be_generic = utf16(&decl(" encoding=\"UTF-16\""), true);
    let le_named = utf16(&decl(" encoding=\"UTF-16LE\""), false);
    let pi = utf16("<?pi x?><r/>", true);
    let contradicted = [
        ("latin.xml", " encoding=\"ISO-8859-1\"", false, "UTF-16LE"),
        ("utf8.xml", " encoding=\"UTF-8\"", true, "UTF-16BE"),
        ("be_named.xml", " encoding=\"UTF-16BE\"", false, "UTF-16LE"),
        ("le_named.xml", " encoding=\"UTF-16LE\"", true, "UTF-16BE"),
    ]
    .map(|(file, encoding, big_endian, order)| {
        (file, utf16(&decl(encoding), big_endian), encoding, order)
    });
    let mut files: Vec<(&str, &[u8])> = vec![
        ("le.xml", &le),
        ("be.xml", &be),
        ("be_generic.xml", &be_generic),
        ("le_named_ok.xml", &le_named),
        ("pi.xml", &pi),
        (
            "both.yaml",
            b"\"#xml\": {bom: UTF-16LE, detected: UTF-16LE}\nr:\n- {}\n",
        ),
        ("bare.yaml", b"\"#xml\": {detected: UTF-16BE}\nr:\n- {}\n"),
        ("wrong.yaml", b"\"#xml\": {detected: UTF-32}\nr:\n- {}\n"),
    ];
    files.extend(
        contradicted
            .iter()
            .map(|(file, bytes, ..)| (*file, bytes.as_slice())),
    );
    let dir = dir_with(&files);
    let json = |fields: &str| {
        format!(
            "{{\n  \"#xml\": {{\n{fields}\n  }},\n  \"r\": [\n    {{\n      \"#text\": \"\u{e9}\"\n    }}\n  ]\n}}\n"
        )
    };
    let mut cases = vec![
        (
            convert("le.xml", "json"),
            ok(json(
                "    \"version\": \"1.0\",\n    \"detected\": \"UTF-16LE\"",
            )),
        ),
        (convert("le.xml", "xml"), ok(le.clone())),
        (
            convert("be.xml", "json"),
            ok(json(
                "    \"version\": \"1.0\",\n    \"detected\": \"UTF-16BE\"",
            )),
        ),
        (convert("be.xml", "xml"), ok(be.clone())),
        (convert("be_generic.xml", "xml"), ok(be_generic.clone())),
        (
            convert("le_named_ok.xml", "json"),
            ok(json(
                "    \"version\": \"1.0\",\n    \"encoding\": \"UTF-16LE\"",
            )),
        ),
        (convert("le_named_ok.xml", "xml"), ok(le_named.clone())),
        (convert("pi.xml", "xml"), ok(pi.clone())),
        (
            convert("both.yaml", "xml"),
            refused(
                "'#xml' has both 'bom' and 'detected', but a file begins with a byte order mark \
                 or does not",
            ),
        ),
        (
            convert("bare.yaml", "xml"),
            refused(
                "'#xml' field 'detected' asks for UTF-16 with no byte order mark, which a reader \
                 can tell only in a file that begins with `<?`: add a declaration or a 'bom'",
            ),
        ),
        (
            convert("wrong.yaml", "xml"),
            refused("'#xml' field 'detected' must be UTF-8, UTF-16LE or UTF-16BE (got \"UTF-32\")"),
        ),
    ];
    for (file, _, encoding, order) in &contradicted {
        let label = encoding.trim_start_matches(" encoding=");
        cases.push((
            convert(file, "json"),
            unreadable(
                file,
                &format!(
                    "the file begins in {order} with no byte order mark but declares encoding \
                     {label}"
                ),
            ),
        ));
    }
    assert_cases(&dir, &cases);
}

/// The reader refuses what the writer would, exit 3, naming it: an
/// undeclared prefix on an element or an attribute, an encoding dq does not
/// know (`UTF-32`, `x-foo`) or cannot read (`ISO-2022-KR`). `xmlns:` and
/// `xml:` — declared or not — keep working, and ISO-2022-JP, which the
/// writer writes, now reads as itself. Before, every refused file read, and
/// could not be written back; ISO-2022-JP read as UTF-8, escapes and all.
#[test]
fn the_reader_refuses_what_the_writer_would() {
    let declared = "<r xmlns:a=\"u\"><a:b a:x=\"1\" xml:lang=\"en\"/></r>";
    let jis = b"<?xml version=\"1.0\" encoding=\"ISO-2022-JP\"?><r>\x1b$B$\"\x1b(B</r>";
    let encoding = |label: &str| format!("<?xml version=\"1.0\" encoding=\"{label}\"?><r/>");
    let (utf32, x_foo, kr) = (
        encoding("UTF-32"),
        encoding("x-foo"),
        encoding("ISO-2022-KR"),
    );
    let dir = dir_with(&[
        ("element.xml", b"<a:b/>"),
        ("attribute.xml", b"<r a:x=\"1\"/>"),
        ("sibling.xml", b"<r><k xmlns:p=\"u\"/><p:k/></r>"),
        ("declared.xml", declared.as_bytes()),
        ("jis.xml", jis),
        ("utf32.xml", utf32.as_bytes()),
        ("x_foo.xml", x_foo.as_bytes()),
        ("kr.xml", kr.as_bytes()),
    ]);
    let prefix = |file: &str, what: &str, prefix: &str| {
        unreadable(
            file,
            &format!(
                "XML parse error: {what} uses the prefix '{prefix}', which no 'xmlns:{prefix}' \
                 declares in scope"
            ),
        )
    };
    let label = |file: &str, label: &str, why: &str| {
        unreadable(
            file,
            &format!("the XML declaration's encoding \"{label}\" is one dq {why}"),
        )
    };
    assert_cases(
        &dir,
        &[
            (
                convert("element.xml", "json"),
                prefix("element.xml", "element <a:b>", "a"),
            ),
            (
                convert("attribute.xml", "json"),
                prefix("attribute.xml", "attribute 'a:x' on <r>", "a"),
            ),
            (
                convert("sibling.xml", "json"),
                prefix("sibling.xml", "element <p:k>", "p"),
            ),
            (
                convert("declared.xml", "xml"),
                ok("<r xmlns:a=\"u\">\n  <a:b a:x=\"1\" xml:lang=\"en\"/>\n</r>"),
            ),
            (
                convert("jis.xml", "json"),
                ok(
                    "{\n  \"#xml\": {\n    \"version\": \"1.0\",\n    \"encoding\": \
                     \"ISO-2022-JP\"\n  },\n  \"r\": [\n    {\n      \"#text\": \"\u{3042}\"\n    \
                     }\n  ]\n}\n",
                ),
            ),
            (convert("jis.xml", "xml"), ok(jis.to_vec())),
            (
                convert("utf32.xml", "json"),
                label("utf32.xml", "UTF-32", "does not know"),
            ),
            (
                convert("x_foo.xml", "json"),
                label("x_foo.xml", "x-foo", "does not know"),
            ),
            (
                convert("kr.xml", "json"),
                label("kr.xml", "ISO-2022-KR", "cannot read"),
            ),
        ],
    );
}

/// A file in 8-bit bytes that declares UTF-16 — .NET's `XmlSerializer`
/// over a `StringWriter` writes `encoding="utf-16"` into a file saved as
/// UTF-8 — reads by its bytes and keeps `#xml.detected: UTF-8`. `-F xml`
/// and `fmt -i` give its bytes back: UTF-8, no BOM, the label as it was.
/// The writer takes `detected: UTF-8` only under a UTF-16 label. Before,
/// the file was rewritten as UTF-16 with a BOM.
#[test]
fn utf8_under_a_utf16_label_is_written_back_as_utf8() {
    let net = "<?xml version=\"1.0\" encoding=\"utf-16\"?><r>\u{e9}</r>";
    let dir = dir_with(&[
        ("net.xml", net.as_bytes()),
        ("in_place.xml", net.as_bytes()),
        (
            "label.yaml",
            b"\"#xml\": {encoding: UTF-16BE, detected: UTF-8}\nr:\n- {}\n",
        ),
        ("no_label.yaml", b"\"#xml\": {detected: UTF-8}\nr:\n- {}\n"),
        (
            "utf8.yaml",
            b"\"#xml\": {encoding: UTF-8, detected: UTF-8}\nr:\n- {}\n",
        ),
    ]);
    assert_cases(
        &dir,
        &[
            (
                convert("net.xml", "json"),
                ok(
                    "{\n  \"#xml\": {\n    \"version\": \"1.0\",\n    \"encoding\": \"utf-16\",\n    \
                    \"detected\": \"UTF-8\"\n  },\n  \"r\": [\n    {\n      \"#text\": \"\u{e9}\"\n    \
                    }\n  ]\n}\n",
                ),
            ),
            (convert("net.xml", "xml"), ok(net)),
            (vec!["fmt", "in_place.xml", "-i"], ok("")),
            (
                convert("label.yaml", "xml"),
                ok("<?xml version=\"1.0\" encoding=\"UTF-16BE\"?><r/>"),
            ),
            (
                convert("no_label.yaml", "xml"),
                refused(
                    "'#xml' field 'detected' is UTF-8, which only a UTF-16 label needs, but there \
                     is no declared encoding",
                ),
            ),
            (
                convert("utf8.yaml", "xml"),
                refused(
                    "'#xml' field 'detected' is UTF-8, which only a UTF-16 label needs, but the \
                     declared encoding is \"UTF-8\"",
                ),
            ),
        ],
    );
    let written = std::fs::read(dir.path().join("in_place.xml")).expect("read in_place.xml");
    assert_eq!(String::from_utf8_lossy(&written), net);
}

/// `xmlns:p=""` undeclares a prefix under Namespaces in XML 1.1, which
/// holds for an XML 1.1 document only: with `version="1.1"` the reader and
/// the writer take it, and the prefix is out of scope inside the element
/// until declared again; with 1.0, or no declaration, both refuse it.
/// Before, the reader took it in either version and the writer refused it
/// in both.
#[test]
fn undeclaring_a_prefix_needs_xml_1_1() {
    let doc = |version: &str, inner: &str| {
        format!(
            "<?xml version=\"{version}\"?><r xmlns:p=\"u\"><k xmlns:p=\"\">{inner}</k><p:k/></r>"
        )
    };
    let v11 = doc("1.1", "");
    let v11_redeclared = doc("1.1", "<j xmlns:p=\"v\"><p:i/></j>");
    let v11_out = doc("1.1", "<j><p:i/></j>");
    let v10 = doc("1.0", "");
    let tree = |version: &str| {
        format!(
            "\"#xml\": {{version: \"{version}\"}}\nr:\n- \"@attrs\": {{\"xmlns:p\": u}}\n  k:\n  - \
             \"@attrs\": {{\"xmlns:p\": \"\"}}\n"
        )
    };
    let (yaml11, yaml10) = (tree("1.1"), tree("1.0"));
    let yaml11_out = format!("{yaml11}    \"p:i\": [{{}}]\n");
    let dir = dir_with(&[
        ("v11.xml", v11.as_bytes()),
        ("v11_redeclared.xml", v11_redeclared.as_bytes()),
        ("v11_out.xml", v11_out.as_bytes()),
        ("v10.xml", v10.as_bytes()),
        ("v11.yaml", yaml11.as_bytes()),
        ("v10.yaml", yaml10.as_bytes()),
        ("v11_out.yaml", yaml11_out.as_bytes()),
        ("bare.yaml", b"r:\n- \"@attrs\": {\"xmlns:p\": \"\"}\n"),
    ]);
    let empty_10 = "'xmlns:p' on <k> is empty: XML namespaces 1.0 cannot undeclare a prefix";
    let undeclared = "element <p:i> uses the prefix 'p', which no 'xmlns:p' declares in scope";
    assert_cases(
        &dir,
        &[
            (
                convert("v11.xml", "xml"),
                ok("<?xml version=\"1.1\"?><r xmlns:p=\"u\">\n  <k xmlns:p=\"\"/>\n  <p:k/>\n</r>"),
            ),
            (
                convert("v11_redeclared.xml", "xml"),
                ok(
                    "<?xml version=\"1.1\"?><r xmlns:p=\"u\">\n  <k xmlns:p=\"\">\n    <j \
                    xmlns:p=\"v\">\n      <p:i/>\n    </j>\n  </k>\n  <p:k/>\n</r>",
                ),
            ),
            (
                convert("v11_out.xml", "json"),
                unreadable("v11_out.xml", &format!("XML parse error: {undeclared}")),
            ),
            (
                convert("v10.xml", "json"),
                unreadable("v10.xml", &format!("XML parse error: {empty_10}")),
            ),
            (
                convert("v11.yaml", "xml"),
                ok("<?xml version=\"1.1\"?><r xmlns:p=\"u\">\n  <k xmlns:p=\"\"/>\n</r>"),
            ),
            (convert("v10.yaml", "xml"), refused(empty_10)),
            (convert("v11_out.yaml", "xml"), refused(undeclared)),
            (
                convert("bare.yaml", "xml"),
                refused("'xmlns:p' on <r> is empty: XML namespaces 1.0 cannot undeclare a prefix"),
            ),
        ],
    );
}

const SET_ZHE: &str = ".r[0].\"#text\" = \"ж\"";

/// `#xml.detected` survives every road that writes a file back: `set
/// --jq` to stdout, `set -i --jq`, `fmt -i` and a bulk `-i` run, which
/// counts every file Modified. A file led by a processing instruction,
/// not a declaration, keeps its byte order too. Before, each BOM-less
/// UTF-16 file came back as UTF-8, and the UTF-8 one under a UTF-16 label
/// as UTF-16 with a BOM.
#[test]
fn detected_survives_set_fmt_and_a_bulk_run() {
    let decl = "<?xml version=\"1.0\"?>";
    let pi = "<?xml-stylesheet type=\"text/xsl\" href=\"s.xsl\"?>";
    let net = |text: &str| format!("<?xml version=\"1.0\" encoding=\"utf-16\"?><r>{text}</r>");
    let le = utf16(&format!("{decl}<r>\u{e9}</r>"), false);
    let be = utf16(&format!("{decl}<r>\u{e9}</r>"), true);
    let pi_be = utf16(&format!("{pi}<r>\u{e9}</r>"), true);
    let net_e = net("\u{e9}");
    let files: [(&str, &[u8]); 5] = [
        ("le.xml", &le),
        ("be.xml", &be),
        ("pi.xml", &pi_be),
        ("net.xml", net_e.as_bytes()),
        ("plain.xml", "<r>\u{e9}</r>".as_bytes()),
    ];
    let dir = dir_with(&files);
    assert_eq!(
        run(&dir, &["set", "le.xml", "--jq", SET_ZHE]),
        (0, utf16(&format!("{decl}<r>ж</r>"), false), String::new())
    );
    assert_eq!(read(&dir, "le.xml"), le);
    for args in [
        ["-i", "set", "be.xml", "--jq", SET_ZHE],
        ["-i", "set", "pi.xml", "--jq", SET_ZHE],
    ] {
        assert_eq!(run(&dir, &args), (0, Vec::new(), String::new()), "{args:?}");
    }
    assert_eq!(
        read(&dir, "be.xml"),
        utf16(&format!("{decl}<r>ж</r>"), true)
    );
    assert_eq!(read(&dir, "pi.xml"), utf16(&format!("{pi}<r>ж</r>"), true));
    assert_eq!(
        run(&dir, &["fmt", "le.xml", "-i"]),
        (0, Vec::new(), String::new())
    );
    assert_eq!(read(&dir, "le.xml"), le);

    let dir = dir_with(&files);
    let (code, stdout, stderr) = run(&dir, &["-i", "set", "*.xml", "--jq", SET_ZHE]);
    assert_eq!(
        (code, String::from_utf8_lossy(&stdout).into_owned(), stderr),
        (
            0,
            "Modified: 5, Skipped: 0, Failed: 0\n".to_owned(),
            String::new()
        )
    );
    assert_eq!(
        ["le.xml", "be.xml", "pi.xml", "net.xml", "plain.xml"].map(|file| read(&dir, file)),
        [
            utf16(&format!("{decl}<r>ж</r>"), false),
            utf16(&format!("{decl}<r>ж</r>"), true),
            utf16(&format!("{pi}<r>ж</r>"), true),
            net("ж").into_bytes(),
            "<r>ж</r>".as_bytes().to_vec(),
        ]
    );
}

/// `detected` is one of three names or null: a number, a boolean, a map or
/// an array is refused naming what it got, a null is no field at all, and
/// `detected: UTF-8` under a label that is not UTF-16 is refused as it is
/// under `UTF-8`.
#[test]
fn detected_is_one_of_three_names_or_null() {
    let tree = |detected: &str| format!("\"#xml\": {{detected: {detected}}}\nr:\n- {{}}\n");
    let files = [
        ("number.yaml", tree("5")),
        ("bool.yaml", tree("true")),
        ("map.yaml", tree("{a: 1}")),
        ("array.yaml", tree("[UTF-8]")),
        ("null.yaml", tree("null")),
        (
            "latin.yaml",
            "\"#xml\": {encoding: ISO-8859-1, detected: UTF-8}\nr:\n- {}\n".to_owned(),
        ),
    ];
    let dir = dir_with(&files.each_ref().map(|(name, src)| (*name, src.as_bytes())));
    let not_a_name = |got: &str| {
        refused(&format!(
            "'#xml' field 'detected' must be UTF-8, UTF-16LE or UTF-16BE (got \"{got}\")"
        ))
    };
    assert_cases(
        &dir,
        &[
            (convert("number.yaml", "xml"), not_a_name("5")),
            (convert("bool.yaml", "xml"), not_a_name("true")),
            (convert("map.yaml", "xml"), not_a_name("object")),
            (convert("array.yaml", "xml"), not_a_name("array")),
            (convert("null.yaml", "xml"), ok("<r/>")),
            (
                convert("latin.yaml", "xml"),
                refused(
                    "'#xml' field 'detected' is UTF-8, which only a UTF-16 label needs, but the \
                     declared encoding is \"ISO-8859-1\"",
                ),
            ),
        ],
    );
}

/// The generic `UTF-16` label starts with the BOM `bom` names, big-endian
/// included, and `UTF-16LE` takes one when `bom` names it.
#[test]
fn a_utf16_label_writes_the_bom_its_bom_field_names() {
    let dir = dir_with(&[
        (
            "generic.yaml",
            b"\"#xml\": {encoding: UTF-16, bom: UTF-16BE}\nr:\n- {}\n",
        ),
        (
            "le.yaml",
            b"\"#xml\": {encoding: UTF-16LE, bom: UTF-16LE}\nr:\n- {}\n",
        ),
    ]);
    let written = |label: &str, big_endian: bool| {
        utf16(
            &format!("<?xml version=\"1.0\" encoding=\"{label}\"?><r/>"),
            big_endian,
        )
    };
    assert_cases(
        &dir,
        &[
            (
                convert("generic.yaml", "xml"),
                ok(with_bom(&[0xFE, 0xFF], written("UTF-16", true))),
            ),
            (
                convert("le.yaml", "xml"),
                ok(with_bom(&[0xFF, 0xFE], written("UTF-16LE", false))),
            ),
        ],
    );
}

/// Every namespace declaration the writer refuses, the reader refuses
/// too, exit 3, on a start tag or an empty one: `xmlns:xmlns`, `xmlns:xml`
/// bound elsewhere, another prefix bound to the `xml` or `xmlns` URI,
/// `xmlns` naming an element, and `xmlns:p=""` with no declaration. Under
/// XML 1.1 the undeclared prefix is out of scope on the undeclaring element
/// itself, for its name and its attributes. A prefix declared on a
/// grandparent is in scope, and the nearest declaration wins. Before, the
/// reader took every one of these.
#[test]
fn the_reader_refuses_every_declaration_the_writer_does() {
    let v11 = "<?xml version=\"1.1\"?>";
    let files = [
        ("xmlns.xml", "<r xmlns:xmlns=\"u\"/>".to_owned()),
        ("xml.xml", "<r xmlns:xml=\"urn:x\"></r>".to_owned()),
        (
            "xml_uri.xml",
            "<r xmlns:p=\"http://www.w3.org/XML/1998/namespace\"></r>".to_owned(),
        ),
        (
            "xmlns_uri.xml",
            "<r xmlns:p=\"http://www.w3.org/2000/xmlns/\"/>".to_owned(),
        ),
        ("element.xml", "<xmlns/>".to_owned()),
        ("prefixed.xml", "<r><xmlns:k></xmlns:k></r>".to_owned()),
        ("empty.xml", "<r xmlns:p=\"\"></r>".to_owned()),
        (
            "self.xml",
            format!("{v11}<r xmlns:p=\"u\"><p:k xmlns:p=\"\"></p:k></r>"),
        ),
        (
            "self_attr.xml",
            format!("{v11}<r xmlns:p=\"u\"><k xmlns:p=\"\" p:a=\"1\"/></r>"),
        ),
        (
            "grandparent.xml",
            "<r xmlns:p=\"u\"><a><b><p:c p:x=\"1\"/></b></a></r>".to_owned(),
        ),
        (
            "nearest.xml",
            "<r xmlns:p=\"u\"><a xmlns:p=\"v\"><p:c/></a></r>".to_owned(),
        ),
    ];
    let dir = dir_with(&files.each_ref().map(|(name, src)| (*name, src.as_bytes())));
    let refused_on_read =
        |file: &str, message: &str| unreadable(file, &format!("XML parse error: {message}"));
    let undeclared = "uses the prefix 'p', which no 'xmlns:p' declares in scope";
    assert_cases(
        &dir,
        &[
            (
                convert("xmlns.xml", "json"),
                refused_on_read(
                    "xmlns.xml",
                    "'xmlns:xmlns' on <r> cannot be declared: the xmlns prefix is reserved",
                ),
            ),
            (
                convert("xml.xml", "json"),
                refused_on_read(
                    "xml.xml",
                    "'xmlns:xml' on <r> must bind http://www.w3.org/XML/1998/namespace (got \
                     \"urn:x\")",
                ),
            ),
            (
                convert("xml_uri.xml", "json"),
                refused_on_read(
                    "xml_uri.xml",
                    "'xmlns:p' on <r> binds http://www.w3.org/XML/1998/namespace, which only the \
                     xml prefix may",
                ),
            ),
            (
                convert("xmlns_uri.xml", "json"),
                refused_on_read(
                    "xmlns_uri.xml",
                    "'xmlns:p' on <r> binds http://www.w3.org/2000/xmlns/, which nothing may",
                ),
            ),
            (
                convert("element.xml", "json"),
                refused_on_read(
                    "element.xml",
                    "'xmlns' cannot name an element: the xmlns prefix is reserved for namespace \
                     declarations",
                ),
            ),
            (
                convert("prefixed.xml", "json"),
                refused_on_read(
                    "prefixed.xml",
                    "'xmlns:k' cannot name an element: the xmlns prefix is reserved for namespace \
                     declarations",
                ),
            ),
            (
                convert("empty.xml", "json"),
                refused_on_read(
                    "empty.xml",
                    "'xmlns:p' on <r> is empty: XML namespaces 1.0 cannot undeclare a prefix",
                ),
            ),
            (
                convert("self.xml", "json"),
                refused_on_read("self.xml", &format!("element <p:k> {undeclared}")),
            ),
            (
                convert("self_attr.xml", "json"),
                refused_on_read(
                    "self_attr.xml",
                    &format!("attribute 'p:a' on <k> {undeclared}"),
                ),
            ),
            (
                convert("grandparent.xml", "xml"),
                ok(
                    "<r xmlns:p=\"u\">\n  <a>\n    <b>\n      <p:c p:x=\"1\"/>\n    </b>\n  \
                     </a>\n</r>",
                ),
            ),
            (
                convert("nearest.xml", "xml"),
                ok("<r xmlns:p=\"u\">\n  <a xmlns:p=\"v\">\n    <p:c/>\n  </a>\n</r>"),
            ),
        ],
    );
}

/// `dq validate` reports on stderr where the reader met an undeclared
/// prefix: the line of the tag and the column just past its end.
#[test]
fn an_undeclared_prefix_is_reported_with_its_line_and_column() {
    let dir = dir_with(&[
        ("start.xml", b"<r>\n  <a:b>t</a:b>\n</r>\n"),
        ("empty.xml", b"<r>\n\n <k a:x=\"1\"/>\n</r>\n"),
    ]);
    let report = |file: &str, line: u32, col: u32, message: &str, snippet: &str| {
        (
            4,
            Vec::new(),
            format!(
                "kind: parse\nfile: {file}\nline: {line}\ncol: {col}\nmessage: XML parse error: \
                 {message}\nsnippet: {snippet}\n"
            ),
        )
    };
    assert_cases(
        &dir,
        &[
            (
                vec!["validate", "start.xml"],
                report(
                    "start.xml",
                    2,
                    8,
                    "element <a:b> uses the prefix 'a', which no 'xmlns:a' declares in scope",
                    "<r>\\n  <a:b>t</a:b>\\n</r>\\n",
                ),
            ),
            (
                vec!["validate", "empty.xml"],
                report(
                    "empty.xml",
                    3,
                    14,
                    "attribute 'a:x' on <k> uses the prefix 'a', which no 'xmlns:a' declares in \
                     scope",
                    "<r>\\n\\n <k a:x=\"1\"/>\\n</r>\\n",
                ),
            ),
        ],
    );
}

/// Every label of the WHATWG `replacement` encoding is one dq cannot read,
/// exit 3, and bytes that are no ISO-2022-JP under that label are a parse
/// error naming it. Before, each read as UTF-8: the labels without a word,
/// the bad bytes as invalid UTF-8.
#[test]
fn replacement_labels_and_bad_iso_2022_jp_are_refused_on_read() {
    let encoding = |label: &str| format!("<?xml version=\"1.0\" encoding=\"{label}\"?><r/>");
    let (hz, kr) = (encoding("hz-gb-2312"), encoding("csiso2022kr"));
    let dir = dir_with(&[
        ("hz.xml", hz.as_bytes()),
        ("kr.xml", kr.as_bytes()),
        (
            "jis.xml",
            b"<?xml version=\"1.0\" encoding=\"ISO-2022-JP\"?><r>\x1b$B\xff\xff\x1b(B</r>",
        ),
    ]);
    let cannot = |file: &str, label: &str| {
        unreadable(
            file,
            &format!("the XML declaration's encoding \"{label}\" is one dq cannot read"),
        )
    };
    assert_cases(
        &dir,
        &[
            (convert("hz.xml", "json"), cannot("hz.xml", "hz-gb-2312")),
            (convert("kr.xml", "json"), cannot("kr.xml", "csiso2022kr")),
            (
                convert("jis.xml", "json"),
                unreadable("jis.xml", "invalid ISO-2022-JP in XML input"),
            ),
        ],
    );
}

/// Whitespace-only text mixing CR, CRLF, space and tab keeps every
/// character through `set` and `set -i`, and a `#text` of U+2028 alone
/// reads back as itself. Before, `set` wrote the CR and LF raw, and the
/// reader took the written text for indentation.
#[test]
fn whitespace_only_text_survives_set_and_in_place() {
    let dir = dir_with(&[
        ("ws.xml", b"<r><d>&#13;&#10; &#13;\t&#10;</d><e>x</e></r>"),
        ("sep.json", "{\"d\":[{\"#text\":\"\u{2028}\"}]}".as_bytes()),
    ]);
    let set_e = ".r[0].e[0].\"#text\" = \"y\"";
    let written = "<r>\n  <d>&#13;&#10;&#32;&#13;&#9;&#10;</d>\n  <e>y</e>\n</r>";
    assert_cases(
        &dir,
        &[
            (vec!["set", "ws.xml", "--jq", set_e], ok(written)),
            (vec!["-i", "set", "ws.xml", "--jq", set_e], ok("")),
            (
                convert("ws.xml", "json"),
                ok(
                    "{\n  \"r\": [\n    {\n      \"d\": [\n        {\n          \"#text\": \
                     \"\\r\\n \\r\\t\\n\"\n        }\n      ],\n      \"e\": [\n        {\n          \
                     \"#text\": \"y\"\n        }\n      ]\n    }\n  ]\n}\n",
                ),
            ),
            (convert("sep.json", "xml"), ok("<d>&#x2028;</d>")),
        ],
    );
    assert_eq!(read(&dir, "ws.xml"), written.as_bytes());
    std::fs::write(dir.path().join("sep.xml"), "<d>&#x2028;</d>").expect("write sep.xml");
    assert_eq!(
        run(&dir, &convert("sep.xml", "json")),
        ok("{\n  \"d\": [\n    {\n      \"#text\": \"\u{2028}\"\n    }\n  ]\n}\n")
    );
}
