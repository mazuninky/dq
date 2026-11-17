//! Regression: `dq fix` legacy `fix.jq` path still works end-to-end.
//!
//! Phase 4 of `add-ir-foundation` introduced `fix.ops` (preferred,
//! comment-preserving) and kept `fix.jq` for backwards compatibility
//! with M10-era rules. The `@std/k8s/image-pull-policy-always` rule was
//! deliberately left on the `fix.jq` path as a coexistence reference;
//! every other `@std/*` namespace either uses `fix.ops` (e.g.
//! `@std/npm/has-license`) or has no fix at all.
//!
//! This test pins the legacy path end-to-end through the CLI: a
//! Deployment fixture with `imagePullPolicy: Always` on a pinned-tag
//! container is rewritten by `dq fix --rules @std/k8s` (the loader
//! uses `@std/<namespace>` not `@std/<namespace>/<rule>`; a sibling
//! rule may also fire but the assertions are scoped to the policy
//! flip). The fix.jq `walk(...)` expression replaces the offending
//! policy with `IfNotPresent`. Comment loss is acceptable here — the
//! legacy path re-emits through `Format::write_with_options`, same
//! trade-off as `dq set --jq`. We assert only that the substantive
//! byte mutation lands.
//!
//! Together with `cli_fix_ops_comment_preservation.rs`, this test
//! covers the Phase 4 spec contract: both branches (jq and ops) work
//! end-to-end and the OPS branch additionally preserves comments.

use std::io::Write;

use clap::Parser;
use tempfile::NamedTempFile;

#[test]
fn dq_fix_with_legacy_fix_jq_rewrites_image_pull_policy() {
    // Deployment fixture: one container with `imagePullPolicy: Always`
    // and a pinned tag (`web:v1.2.3`). The `@std/k8s/image-pull-policy-always`
    // check fires for the pinned-tag-with-Always combination; its
    // `fix.jq` walks the value tree and swaps `Always` →
    // `IfNotPresent`.
    let doc_yaml = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: web
          image: web:v1.2.3
          imagePullPolicy: Always
";
    let mut doc_tmp = NamedTempFile::with_suffix(".yaml").expect("doc tempfile");
    doc_tmp
        .write_all(doc_yaml.as_bytes())
        .expect("write doc yaml");
    let doc_path = doc_tmp.into_temp_path();

    let cli = dq::Cli::try_parse_from([
        "dq",
        "-i",
        "--no-color",
        "fix",
        "--rules",
        "@std/k8s",
        doc_path.to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    assert!(
        result.is_ok(),
        "dq fix must succeed on the legacy fix.jq path; got err={result:?}, stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err),
    );

    let post = std::fs::read_to_string(&doc_path).expect("read post-fix yaml");

    // Substantive contract: the offending `Always` flipped to
    // `IfNotPresent`, the rule fired, and the file was rewritten.
    assert!(
        post.contains("imagePullPolicy: IfNotPresent"),
        "legacy fix.jq must rewrite imagePullPolicy → IfNotPresent; got:\n{post}",
    );
    assert!(
        !post.contains("imagePullPolicy: Always"),
        "no `imagePullPolicy: Always` should remain after the fix; got:\n{post}",
    );

    // Sanity: every other key the rule didn't touch is still present.
    // We only check the structural ones; comment loss is acceptable on
    // the legacy path because `dq fix` re-emits via
    // `Format::write_with_options` for any rule that took the JQ
    // branch.
    for marker in [
        "kind: Deployment",
        "name: web",
        "image: web:v1.2.3",
        "replicas: 3",
    ] {
        assert!(
            post.contains(marker),
            "marker {marker:?} must survive the legacy fix.jq re-emit; got:\n{post}",
        );
    }

    // -i mode writes to disk; stdout is empty.
    assert!(
        out.is_empty(),
        "stdout must be empty under -i; got: {:?}",
        String::from_utf8_lossy(&out),
    );
}

#[test]
fn dq_fix_legacy_fix_jq_is_idempotent_on_already_fixed_doc() {
    // Run #1 fixes the doc; run #2 sees a conformant doc and must
    // leave it untouched. Pins the M10 idempotency contract still
    // holds for the legacy path after Phase 4.
    let doc_yaml = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  template:
    spec:
      containers:
        - name: web
          image: web:v1.2.3
          imagePullPolicy: IfNotPresent
";
    let mut doc_tmp = NamedTempFile::with_suffix(".yaml").expect("doc tempfile");
    doc_tmp
        .write_all(doc_yaml.as_bytes())
        .expect("write doc yaml");
    let doc_path = doc_tmp.into_temp_path();

    let cli = dq::Cli::try_parse_from([
        "dq",
        "-i",
        "--no-color",
        "fix",
        "--rules",
        "@std/k8s",
        doc_path.to_str().expect("UTF-8 doc path"),
    ])
    .expect("clap parse");

    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let result = dq::run(&cli, false, &mut out, &mut err);
    assert!(
        result.is_ok(),
        "dq fix on a conformant doc must succeed; got err={result:?}, stderr={:?}",
        String::from_utf8_lossy(&err),
    );

    let post = std::fs::read_to_string(&doc_path).expect("read post-fix yaml");
    assert!(
        post.contains("imagePullPolicy: IfNotPresent"),
        "conformant doc must keep IfNotPresent intact; got:\n{post}",
    );
    assert!(
        !post.contains("imagePullPolicy: Always"),
        "no `Always` should appear in either the input or the output; got:\n{post}",
    );
}
