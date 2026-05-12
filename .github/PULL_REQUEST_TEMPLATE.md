<!--
Thanks for contributing to dq! A few pointers before you submit:

- For non-trivial changes (new format, new rule semantics, new subcommand),
  open an OpenSpec change under openspec/changes/<name>/ first so we can align
  on scope. See CLAUDE.md for the workflow.
- Keep the PR focused on one logical change.
- CI must be green (cargo fmt, cargo clippy -D warnings, cargo test on
  Linux + macOS, cargo deny, cargo audit). See .github/workflows/ci.yml.
-->

## Summary

<!-- What does this PR do and why? One or two paragraphs. -->

## Related issues / specs

<!-- e.g. Fixes #123, Refs #456, Refs openspec/changes/<name>. Delete if not applicable. -->

## Changes

<!-- Optional bullet list of the main things this PR touches. -->

-
-

## Testing

<!-- How did you verify the change? -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --workspace --all-features`
- [ ] Manually exercised the affected command(s): <!-- which? -->

## Notes for reviewers

<!-- Anything reviewers should pay extra attention to, or parts you are unsure about. Delete if none. -->
