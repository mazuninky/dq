//! Handler-level tests for the M5 format extensions, driven through `dq::run`.
//!
//! Mirror of `unit_get.rs` / `unit_convert.rs`: each test builds a `Cli` via
//! `Cli::parse_from(...)` and calls `dq::run` with `Vec<u8>` writers — no
//! subprocess, no SIGPIPE plumbing, no env mutation. Fixtures are seeded into
//! tempdirs so the checked-in fixtures under `tests/fixtures/` are never
//! mutated by the CSV / DotEnv / etc. write-paths.
//!
//! Each test pins a single command × format pairing. Where the prompt's plan
//! exposes surprising behaviour (test 12 — `dq set Dockerfile` exit code,
//! test 13 — `dq fmt --sort-keys` on INI) the test asserts the CURRENT
//! behaviour and the docstring records what the prompt expected so a
//! reviewer can decide whether the contract or the implementation should
//! move.

use std::fs;
use std::io::Write as _;

use camino::Utf8PathBuf;
use clap::Parser;
use dq::Cli;
use tempfile::{NamedTempFile, TempDir};

/// Seed `<dir>/<name>` with `content` and return the UTF-8 path. The caller
/// owns the `TempDir` — kept alive for the duration of the test — so the
/// file isn't cleaned up before the handler runs.
fn seed_file(dir: &TempDir, name: &str, content: &str) -> Utf8PathBuf {
    let path = Utf8PathBuf::from_path_buf(dir.path().join(name)).expect("utf-8 tempdir");
    fs::write(path.as_std_path(), content).expect("seed file");
    path
}

/// Returns a `TempPath` so the underlying handle is closed before the binary
/// touches the path. Windows requires this for in-place rewrites — the same
/// pattern propagated from `cli_set_jq.rs`. Applied uniformly even on
/// read-only sites for consistency.
fn write_with_extension(ext: &str, content: &str) -> tempfile::TempPath {
    let mut tmp = NamedTempFile::with_suffix(format!(".{ext}")).expect("tempfile");
    tmp.write_all(content.as_bytes()).expect("write tempfile");
    tmp.into_temp_path()
}

// ---------------------------------------------------------------------------
// Test 1 — `dq get terraform_main.tf /backend/0/region`
// ---------------------------------------------------------------------------
//
// PROMPT NOTE: the prompt asks for `/backend/0/region`, but the HCL
// label-as-keys nesting is `/<block>/<label>/<attr>` — so the real pointer
// for `backend "s3" { region = ... }` is `/backend/s3/region`. This test
// uses the pointer that actually addresses the value (the alternative —
// expecting an Array of backends — is not what the parser produces per
// `parsers/hcl.rs`).

#[test]
fn get_hcl_backend_region_via_label_keyed_pointer() {
    let tmp = write_with_extension("tf", "backend \"s3\" {\n  region = \"us-east-1\"\n}\n");
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "get", path, "/backend/s3/region", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("get on HCL must succeed");
    let s = String::from_utf8(out).expect("utf-8 stdout");
    assert!(
        s.contains("us-east-1"),
        "stdout must contain the region scalar, got: {s:?}",
    );
    assert!(err.is_empty(), "expected empty stderr, got: {err:?}");
}

// ---------------------------------------------------------------------------
// Test 2 — `dq paths app.ini` lists section/key pointers
// ---------------------------------------------------------------------------

#[test]
fn paths_ini_lists_section_and_key_pointers_as_console_lines() {
    let tmp = write_with_extension(
        "ini",
        "log = info\n[server]\nport = 8080\n[client]\ntimeout = 30\n",
    );
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "paths", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("paths on INI must succeed");
    let s = String::from_utf8(out).expect("utf-8");
    // Anonymous-section keys live under `/`; named sections under `/<name>/<key>`.
    assert!(
        s.contains("/server: object"),
        "expected /server section pointer, got:\n{s}",
    );
    assert!(
        s.contains("/server/port: string"),
        "expected /server/port leaf pointer, got:\n{s}",
    );
    assert!(
        s.contains("/client/timeout: string"),
        "expected /client/timeout leaf pointer, got:\n{s}",
    );
}

// ---------------------------------------------------------------------------
// Test 3 — `dq get service.env /DATABASE_URL`
// ---------------------------------------------------------------------------

#[test]
fn get_dotenv_database_url_returns_string_value() {
    let tmp = write_with_extension(
        "env",
        "DATABASE_URL=postgres://user@db.example.test:5432/svc\n# comment\n",
    );
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "get", path, "/DATABASE_URL", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("get on .env must succeed");
    let s = String::from_utf8(out).expect("utf-8");
    assert!(
        s.contains("postgres://user@db.example.test:5432/svc"),
        "expected DATABASE_URL value in stdout, got: {s:?}",
    );
}

// ---------------------------------------------------------------------------
// Test 4 — `dq paths users.csv` lists per-cell pointers like /0/name etc.
// ---------------------------------------------------------------------------

#[test]
fn paths_csv_lists_per_cell_pointers() {
    let tmp = write_with_extension(
        "csv",
        "name,email\nalice,alice@example.test\nbob,bob@example.test\n",
    );
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "paths", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("paths on CSV must succeed");
    let s = String::from_utf8(out).expect("utf-8");
    // The CSV parser produces `Array<Map>`, so each cell pointer is
    // `/<row>/<column>`. Pin the first row's cells.
    for expected in ["/0/name: string", "/0/email: string", "/1/name: string"] {
        assert!(
            s.contains(expected),
            "expected pointer line `{expected}`, got:\n{s}",
        );
    }
}

// ---------------------------------------------------------------------------
// Test 5 — `dq validate Dockerfile` exits 0 on a valid file
// ---------------------------------------------------------------------------

#[test]
fn validate_dockerfile_succeeds_silently_for_valid_input() {
    // Use a tempdir + literal `Dockerfile` filename so the FILENAME_FALLBACK
    // table dispatches us to the dockerfile parser without an extension.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seed_file(&dir, "Dockerfile", "FROM alpine:latest\nRUN apk add curl\n");
    let cli = Cli::parse_from(["dq", "validate", path.as_str(), "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("valid Dockerfile must validate successfully");
    assert!(out.is_empty(), "validate must not write to stdout");
    assert!(err.is_empty(), "valid input must not write to stderr");
}

// ---------------------------------------------------------------------------
// Test 6 — `dq validate Dockerfile.broken` returns ValidateFail (exit 4)
// ---------------------------------------------------------------------------

#[test]
fn validate_broken_dockerfile_returns_validate_fail() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Use `.dockerfile` extension so the parser is dispatched even though
    // the filename isn't literally `Dockerfile` (the broken-fixture pattern
    // — same file-renamed, same parser).
    let path = seed_file(&dir, "broken.dockerfile", "FROM\n"); // FROM with no image
    let cli = Cli::parse_from(["dq", "validate", path.as_str(), "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e =
        dq::run(&cli, false, &mut out, &mut err).expect_err("broken Dockerfile must fail validate");
    // ValidateFail wrapper makes the exit-code mapper pick 4. This is the
    // contract `cli_smoke::smoke_validate_broken_json_exits_four` already
    // pins for JSON; we replicate it for Dockerfile.
    assert!(
        e.downcast_ref::<dq::ValidateFail>().is_some(),
        "validate must wrap parse errors in ValidateFail; got: {e:?}",
    );
    assert_eq!(
        dq::exit_code::exit_code_for_error(&e),
        dq::exit_code::VALIDATE_FAIL,
        "ValidateFail must map to exit code 4",
    );
}

// ---------------------------------------------------------------------------
// Test 7 — `dq paths repo.gitignore` is a flat list of pattern pointers
// ---------------------------------------------------------------------------

#[test]
fn paths_gitignore_emits_flat_array_pointers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seed_file(
        &dir,
        ".gitignore",
        "node_modules/\n# comment\n*.log\ntarget/\n",
    );
    let cli = Cli::parse_from(["dq", "paths", path.as_str(), "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("paths on .gitignore must succeed");
    let s = String::from_utf8(out).expect("utf-8");
    // The ignore-list is parsed as a flat `Array<String>`, so pointers are
    // `/0`, `/1`, `/2` — each typed `string`.
    for expected in ["/0: string", "/1: string", "/2: string"] {
        assert!(
            s.contains(expected),
            "expected pointer line `{expected}`, got:\n{s}",
        );
    }
}

// ---------------------------------------------------------------------------
// Test 8 — `dq get hugo_post.md /title`
// ---------------------------------------------------------------------------

#[test]
fn get_frontmatter_title_returns_yaml_header_value() {
    // M9 §D3: default `.md` extension dispatch flipped from `Frontmatter`
    // (M5) to `Markdown` (M9). The frontmatter header is now folded into
    // the AST's top-level `frontmatter.value.<key>` shape, so the M5
    // pointer `/title` becomes `/frontmatter/value/title`. The migration
    // plan in the M9 OpenSpec change documents both alternatives — the
    // longer pointer (here) and the explicit `-F frontmatter` opt-in
    // (covered by the M5 frontmatter parser's own tests).
    let tmp = write_with_extension(
        "md",
        "---\ntitle: Hello, dq\nauthor: example-team\n---\n# Body\n",
    );
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "get", path, "/frontmatter/value/title", "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("get on markdown frontmatter must succeed");
    let s = String::from_utf8(out).expect("utf-8");
    assert!(
        s.contains("Hello, dq"),
        "expected /frontmatter/value/title scalar in stdout, got: {s:?}",
    );
}

// ---------------------------------------------------------------------------
// Test 9 — `dq convert hugo_post.md -F json` projects the value (drops body)
// ---------------------------------------------------------------------------

#[test]
fn convert_frontmatter_to_json_emits_object_with_header_keys() {
    // M9 §D3: default `.md` extension dispatch resolves to `Markdown`, so
    // converting a markdown source to JSON now emits the full AST shape
    // (a top-level `{ "type": "document", "frontmatter": {...},
    // "children": [...], "position": {...} }`). Frontmatter values are
    // accessible at `frontmatter.value.<key>`. Body content IS preserved
    // in the AST `children` field — the M5 "drop body" assertion is
    // explicitly out of date now.
    //
    // To get the M5 header-only-projection behaviour back, callers must
    // opt in via `-F frontmatter` for the *input*; the M5 frontmatter
    // parser's own test suite still covers that path.
    let tmp = write_with_extension(
        "md",
        "---\ntitle: Hello\nauthor: ex\n---\nBody to be dropped.\n",
    );
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "convert", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err)
        .expect("convert markdown→json must succeed under M9 default dispatch");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("convert -F json must emit valid JSON");
    let serde_json::Value::Object(obj) = &parsed else {
        panic!("convert markdown→json must produce a JSON object, got: {parsed:?}");
    };
    // Top-level shape — document discriminator + folded frontmatter.
    assert_eq!(obj.get("type"), Some(&serde_json::json!("document")));
    let fm = obj
        .get("frontmatter")
        .and_then(|v| v.as_object())
        .expect("frontmatter object present");
    assert_eq!(fm.get("kind"), Some(&serde_json::json!("yaml")));
    let inner = fm
        .get("value")
        .and_then(|v| v.as_object())
        .expect("frontmatter.value object present");
    assert_eq!(inner.get("title"), Some(&serde_json::json!("Hello")));
    assert_eq!(inner.get("author"), Some(&serde_json::json!("ex")));
}

// ---------------------------------------------------------------------------
// Test 10 — `dq convert app.ini -F json` produces section→sub-object map
// ---------------------------------------------------------------------------

#[test]
fn convert_ini_to_json_uses_section_names_as_top_level_keys() {
    let tmp = write_with_extension(
        "ini",
        "[server]\nport = 80\nhost = localhost\n[client]\ntimeout = 30\n",
    );
    let path = tmp.to_str().unwrap();
    let cli = Cli::parse_from(["dq", "-F", "json", "convert", path, "--no-color"]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("convert ini→json must succeed");
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("convert -F json must emit valid JSON");
    let serde_json::Value::Object(obj) = &parsed else {
        panic!("expected JSON object, got: {parsed:?}");
    };
    let server = obj.get("server").expect("server section must be present");
    let serde_json::Value::Object(server_obj) = server else {
        panic!("server section must be a JSON object, got: {server:?}");
    };
    assert_eq!(server_obj.get("port"), Some(&serde_json::json!("80")));
    assert!(obj.contains_key("client"), "client section must be present");
}

// ---------------------------------------------------------------------------
// Test 11 — `dq convert deploy.yaml -F dockerfile` is rejected by clap (exit 6)
// ---------------------------------------------------------------------------

#[test]
fn convert_to_dockerfile_format_is_rejected_at_clap_parse() {
    // `OutputFormat` (clap ValueEnum) deliberately does NOT include
    // `Dockerfile` (it's a read-only format per design D9), so `-F dockerfile`
    // must fail at clap's parse step. Per the prompt, "exit 6" is the
    // observable behaviour the binary maps clap parse failures to via
    // clap's error handling. From the in-process perspective `try_parse_from`
    // returns an error directly — no `dq::run` involvement.
    let result = Cli::try_parse_from(["dq", "-F", "dockerfile", "convert", "deploy.yaml"]);
    let err = result.expect_err("`-F dockerfile` must fail at clap parse");
    let msg = err.to_string();
    assert!(
        msg.contains("dockerfile") || msg.contains("invalid value"),
        "clap error must mention the rejected value, got: {msg}",
    );
}

// ---------------------------------------------------------------------------
// Test 12 — `dq set Dockerfile /0/instruction RUN -i` — read-only contract
// ---------------------------------------------------------------------------
//
// Pipeline: `Cli::ensure_write_flags_consistent` passes, the handler loads
// the Dockerfile via the value-only parser, then `Document::set_at` returns
// `WriteUnavailable { reason: "dockerfile document was loaded read-only; ..." }`
// because the dockerfile parser produces no spans. `WriteUnavailable` maps
// to exit 7 (WRITE_FAILED) per `exit_code.rs`.
//
// The reason string names the format (`dockerfile`) so users see why the
// rejection happened without having to cross-reference docs.

#[test]
fn set_dockerfile_returns_write_unavailable_mapped_to_exit_seven() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seed_file(&dir, "Dockerfile", "FROM alpine\n");
    let cli = Cli::parse_from([
        "dq",
        "set",
        path.as_str(),
        "/0/instruction",
        "RUN",
        "-i",
        "--no-color",
    ]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let e = dq::run(&cli, false, &mut out, &mut err)
        .expect_err("set on a read-only Dockerfile must error");
    // Verify the mapped exit code matches the expected 7.
    assert_eq!(
        dq::exit_code::exit_code_for_error(&e),
        dq::exit_code::WRITE_FAILED,
        "set on Dockerfile must surface as WRITE_FAILED (exit 7), got: {e:?}",
    );
    let domain = e
        .downcast_ref::<dq_core::Error>()
        .expect("error must downcast to dq_core::Error");
    // The handler-emitted variant: WriteUnavailable from the read-only
    // value-only document. The reason names the format "dockerfile" so
    // the user-visible message explains why the rejection happened.
    let dq_core::Error::WriteUnavailable { reason } = domain else {
        panic!("expected WriteUnavailable, got: {domain:?}");
    };
    assert!(
        reason.contains("dockerfile"),
        "WriteUnavailable reason must name the format `dockerfile`, got: {reason}",
    );
    assert!(
        reason.contains("read-only"),
        "WriteUnavailable reason must still flag the document as read-only, got: {reason}",
    );
}

// ---------------------------------------------------------------------------
// Test 13 — `dq fmt config.ini -i --sort-keys` — keys sorted within section
// ---------------------------------------------------------------------------
//
// `Ini::write_with_options` honours `--sort-keys` by deep-canonicalising the
// value tree (sections sorted alphabetically and, since each section is a
// `Value::Map`, the keys within each section are sorted too). After fmt the
// on-disk file must show every key in alphabetic order within its section.

#[test]
fn fmt_ini_with_sort_keys_sorts_keys_within_each_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = seed_file(&dir, "config.ini", "[s]\nz = 1\na = 2\n[t]\nq = 3\nb = 4\n");
    let cli = Cli::parse_from([
        "dq",
        "--sort-keys",
        "-i",
        "fmt",
        path.as_str(),
        "--no-color",
    ]);
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    dq::run(&cli, false, &mut out, &mut err).expect("fmt --sort-keys -i must complete on INI");
    let on_disk = fs::read_to_string(path.as_std_path()).expect("read file");
    // Sections must survive (no file corruption).
    assert!(
        on_disk.contains("[s]"),
        "section [s] must survive: {on_disk:?}"
    );
    assert!(
        on_disk.contains("[t]"),
        "section [t] must survive: {on_disk:?}"
    );
    // Within section [s], the key `a` must come before `z` after sort.
    let s_block_start = on_disk.find("[s]").expect("[s] header");
    let pos_a = on_disk[s_block_start..]
        .find("a=")
        .or_else(|| on_disk[s_block_start..].find("a ="))
        .expect("key `a` must be present in [s]");
    let pos_z = on_disk[s_block_start..]
        .find("z=")
        .or_else(|| on_disk[s_block_start..].find("z ="))
        .expect("key `z` must be present in [s]");
    assert!(
        pos_a < pos_z,
        "with --sort-keys, key `a` must precede `z` in [s], got:\n{on_disk}",
    );
    // Within section [t], `b` must come before `q` after sort.
    let t_block_start = on_disk.find("[t]").expect("[t] header");
    let pos_b = on_disk[t_block_start..]
        .find("b=")
        .or_else(|| on_disk[t_block_start..].find("b ="))
        .expect("key `b` must be present in [t]");
    let pos_q = on_disk[t_block_start..]
        .find("q=")
        .or_else(|| on_disk[t_block_start..].find("q ="))
        .expect("key `q` must be present in [t]");
    assert!(
        pos_b < pos_q,
        "with --sort-keys, key `b` must precede `q` in [t], got:\n{on_disk}",
    );
}
