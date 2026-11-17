# path-syntax Specification

## Purpose

Defines the path/query languages accepted by `dq` commands in M1: RFC 6901 JSON Pointer for navigation commands (`get`, `exists`, `keys`, `values`, `len`, `type`) and RFC 9535 JSONPath restricted to the `select` subcommand. Also covers path-related diagnostics: `did_you_mean` suggestions and canonical pointer rendering in errors. jq is explicitly deferred.

## Requirements

### Requirement: JSON Pointer (RFC 6901) parser

`dq-core` SHALL provide a typed `Pointer` value that parses RFC 6901 syntax: empty string addresses the root document, `/foo/bar` addresses nested keys, `/0` addresses array index 0, `~0` un-escapes to `~`, and `~1` un-escapes to `/`.

#### Scenario: Root pointer
- **WHEN** the user runs `dq get config.yaml ''`
- **THEN** the entire document is returned in the requested output format

#### Scenario: Escaped slash in key
- **WHEN** the user runs `dq get manifest.yaml '/metadata/labels/app.kubernetes.io~1name'` against a manifest with label `app.kubernetes.io/name`
- **THEN** the command resolves the key correctly and writes the label value to stdout

#### Scenario: Escaped tilde in key
- **WHEN** the user runs `dq get config.yaml '/keys/key~0with~1tilde'` against a document with key `key~with/tilde`
- **THEN** the command resolves the key correctly

### Requirement: Pointer navigation against `Document`

`dq-core` SHALL expose `Pointer::resolve(&Document) -> Result<&Value>` that returns either the addressed node or a structured `Error` with `kind=path`, the original pointer, the longest matching prefix, and (when sibling keys are within Levenshtein distance ≤ 2 of the missing segment) a `did_you_mean` field listing up to three candidates.

#### Scenario: Successful navigation
- **WHEN** `Pointer::resolve` is called with `/server/port` against a document where `server.port = 8080`
- **THEN** the function returns `Ok(&Value::Int(8080))`

#### Scenario: Error suggests close key
- **WHEN** `Pointer::resolve` is called with `/server/prot` against a document where `server.port = 8080`
- **THEN** the returned error includes `did_you_mean: ["port"]`, `matched_prefix: "/server"`, and `kind: path`

#### Scenario: Error reports correct prefix
- **WHEN** `Pointer::resolve` is called with `/server/tls/cert` and `server.tls` does not exist
- **THEN** the returned error has `matched_prefix: "/server"` (the deepest existing prefix) and the missing segment `tls`

### Requirement: JSONPath (RFC 9535) for `select` only

`dq-cli` SHALL accept RFC 9535 JSONPath expressions only in the `select` subcommand. The `get`, `set`, `del`, etc. commands SHALL accept only RFC 6901 JSON Pointers.

#### Scenario: JSONPath in `select`
- **WHEN** the user runs `dq select manifest.yaml '$.spec.containers[*].image'`
- **THEN** the JSONPath engine returns all matching values as a JSON array

#### Scenario: JSONPath in `get` rejected
- **WHEN** the user runs `dq get manifest.yaml '$.spec.replicas'`
- **THEN** the command writes a structured error stating that `get` accepts only RFC 6901 JSON Pointer (suggested fix: `dq get manifest.yaml /spec/replicas`) and exits with code 1

### Requirement: jq expressions deferred to M7

`dq-cli` MUST NOT accept jq expressions in any M1 command. The `query` subcommand and the `--jq` global flag MUST be hidden in `--help` (or absent from the M1 binary) and MUST emit an "unsupported in this build" error if invoked.

#### Scenario: query command unavailable
- **WHEN** the user runs `dq query foo.json '.bar'`
- **THEN** the command exits with code 1 and a message indicating that jq support arrives in M7

### Requirement: Pointer rendering for diagnostics

When a structured error references a path, the renderer SHALL output the pointer in canonical RFC 6901 form (re-escaping `~` and `/`) so users can copy-paste it back into a command.

#### Scenario: Renderer escapes slashes in suggestions
- **WHEN** an error includes a `did_you_mean` suggestion for the key `app.kubernetes.io/name`
- **THEN** the rendered suggestion text contains `app.kubernetes.io~1name`, not the literal slash
