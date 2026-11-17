# Security Policy

## Reporting a vulnerability

**Please do not open a public issue or pull request for security reports.**

If you believe you have found a security vulnerability in `dq`, report it privately through [GitHub Security Advisories](https://github.com/mazuninky/dq/security/advisories/new). This opens a confidential channel where we can discuss the details and coordinate a fix before any public disclosure.

If you cannot use GitHub Security Advisories, email **mazuninky@gmail.com** with:

- A description of the issue and its impact.
- Steps to reproduce, ideally with a minimal proof of concept (a small input file and the `dq` command line that triggers it).
- The affected `dq` version (`dq --version`) and the OS / architecture.

You should receive an acknowledgement within a few days. We will work with you on a fix and a disclosure timeline, and credit you in the release notes unless you prefer to remain anonymous.

## Scope

`dq` is a CLI that reads, edits, and lints structured data files, plus an optional WASM plugin runtime. Security-relevant surfaces are:

- **Parser correctness.** YAML, JSON, TOML, HCL, XML, INI, CSV, Markdown, Dockerfile parsers. Crashes, infinite loops, or out-of-bounds reads on hostile inputs are in scope.
- **File I/O.** Reading files passed by `<FILE|GLOB>`, atomic-write tempfile + rename in `-i` mode, `$ref` and `schema_file` path resolution in rules (must reject `..` escapes and absolute paths).
- **Self-update mechanism.** `dq self update` downloads and replaces the running binary from the `mazuninky/dq` GitHub Releases endpoint, verifies SHA256, and performs an atomic rename. Any path that lets an attacker substitute the downloaded binary or bypass the checksum is in scope.
- **WASM plugin sandbox.** When `--features plugins` is enabled, the `wasmtime` runtime loads `*.wasm` files from `--plugins <DIR>` without WASI: no network, no filesystem, no processes. Fuel budget ~1 s CPU, memory cap 64 MiB. A plugin escaping the sandbox or stealing host resources beyond these bounds is in scope.
- **Embedded `jq` evaluator (`jaq`).** Used by `dq query`, `dq set --jq`, and rule `check.jq` / `fix.jq` blocks. Memory or CPU exhaustion from a hostile rule file is in scope when the user is reasonably expected to run an untrusted ruleset (e.g. shared CI).
- **Composite rules.** `extract:` + reparse + `nested:` recursion is bounded at `MAX_EXTRACT_DEPTH = 4`. Anything that bypasses that bound or escalates an inner-format parser bug is in scope.

Vulnerabilities in upstream Rust crates (`saphyr`, `quick-xml`, `jaq`, `wasmtime`, etc.) are out of scope here — report those upstream. Where `dq`'s configuration of those crates makes a known-safe upstream feature unsafe, that's our bug.

## How `dq` handles secrets

`dq` does not own user credentials. A few properties worth knowing when evaluating impact:

- `dq` accepts no API tokens or passwords. The CLI itself never reads credentials from arguments or environment. The only network call it makes is to `api.github.com` and `objects.githubusercontent.com` during `dq self check` / `dq self update`.
- `dq` does not write to global state outside the workspace it was invoked in. Atomic writes target the same directory as the file being written. Tempfiles are removed on failure.
- The plugin sandbox forbids network, filesystem, and process access. A plugin cannot exfiltrate data through any host-provided import.

If you find a case where `dq` reads, writes, or transmits data outside the explicit file paths it was asked to handle, please report it.

## Dependencies

We track dependency advisories via [`cargo audit`](https://github.com/rustsec/cargo-audit) on a daily schedule ([`.github/workflows/audit.yml`](workflows/audit.yml)) and via [Dependabot](dependabot.yml). A CVE in one of our dependencies is **not** automatically a vulnerability in `dq`; whether it is exploitable depends on how we use the affected code path. When you report a dependency-based issue, please include a call chain or proof-of-concept showing that `dq` is actually reachable.

## Public disclosure

We prefer coordinated disclosure. Once a fix is released, we will publish a security advisory describing the issue, the affected versions, and the upgrade path.
