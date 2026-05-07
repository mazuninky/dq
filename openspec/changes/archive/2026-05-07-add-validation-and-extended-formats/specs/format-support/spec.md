## MODIFIED Requirements

### Requirement: M1 anti-scope for formats

The crate SHALL NOT include parsers for the conftest-only formats (CUE, EDN, Jsonnet, HOCON, nginx, SPDX, TextProto, VCL); those remain anti-scope per [dq-plan.md:600-612](../../../dq-plan.md:600). XSD / RelaxNG / Schematron schema-validators are also anti-scope (covered separately if/when a use case appears).

The earlier wording deferring **XML write** to M11 is now superseded: this change adds full XML read+write through the `XmlFormat` requirement below. The earlier wording deferring the M9 markdown body parser is also obsolete (already shipped in M9).

#### Scenario: Unsupported format error
- **WHEN** the user runs `dq get script.sh /x` (no registered format for `.sh`)
- **THEN** the command writes a structured error suggesting `-F <fmt>` and exits with code 1

### Requirement: New `FormatTag` variants

`Document::FormatTag` SHALL gain a `Xml` variant (in addition to the M5 variants `Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Dockerfile`, `IgnoreList`, `Frontmatter`). `FormatTag::from_name` SHALL recognise the lowercase name `"xml"` for the new tag.

#### Scenario: from_name maps `xml`
- **WHEN** the caller invokes `FormatTag::from_name("xml")`
- **THEN** the result is `Some(FormatTag::Xml)`

#### Scenario: from_name still maps M5 tags
- **WHEN** the caller invokes `FormatTag::from_name("frontmatter")`
- **THEN** the result is `Some(FormatTag::Frontmatter)`

### Requirement: New `OutputFormat` write-target variants

`crates/dq-cli/src/output/mod.rs::OutputFormat` SHALL gain a `Xml` variant in addition to the M5 set (`Hcl`, `Ini`, `DotEnv`, `Csv`, `Tsv`, `Frontmatter`). `OutputFormat::Dockerfile` and `OutputFormat::IgnoreList` SHALL NOT exist — clap rejects `-F dockerfile` / `-F ignore-list` at the parse step (exit 6). `dq convert <input> -F xml` SHALL be accepted and route through `XmlFormat::write_with_options`.

#### Scenario: `convert -F xml` is accepted
- **WHEN** the user runs `dq convert app.json -F xml`
- **THEN** the command exits 0 and stdout contains a well-formed XML document built from the JSON via the conventional-key mapping (see XML support requirement)

#### Scenario: `convert -F dockerfile` still rejected
- **WHEN** the user runs `dq convert app.json -F dockerfile`
- **THEN** clap rejects the value at the parse step (exit 6)

## ADDED Requirements

### Requirement: XML read and write support via `quick-xml`

`dq-core` SHALL parse and write XML 1.0 documents through a new `XmlFormat` implementation that depends on `quick-xml = "0.36"` (with the `serialize` feature). XML documents map onto the existing `Document::Value` enum using **conventional keys** rather than introducing a new `Value` variant:

| XML construct                         | `Value` mapping                                                  |
|---------------------------------------|------------------------------------------------------------------|
| `<tag>` element                        | `Map { tag => Array<Map { ... }> }` on the parent                |
| Attributes                             | `Map { "@attrs" => Map { name => string, ... } }` on the element |
| Text content                           | `String` under key `"#text"` on the element                      |
| `<!-- comment -->`                     | `Array<String>` under key `"#comments"` on the parent element    |
| `<![CDATA[...]]>` block                 | `Array<String>` under key `"#cdata"` on the element              |
| `<?xml-stylesheet ...?>` PI            | `Array<String>` under key `"#pi"` on the parent element          |
| `<?xml version="1.0" encoding="..."?>` | `Map { "version", "encoding", "standalone" }` under top-level key `"#xml"` |
| Namespace prefix on tag (`foo:tag`)    | retained in the tag name string verbatim (`"foo:tag"`)           |
| `xmlns:foo` attribute                  | retained in `@attrs` verbatim                                    |

Multi-element children with the same tag are stored as a single `Array` to preserve order; even single occurrences are wrapped in a one-element array so `Pointer` indexing is stable across `<a><b/></a>` and `<a><b/><b/></a>`.

The `XmlFormat::write` round-trip is **partial**: element structure, attributes, comments, CDATA, processing instructions, namespace prefixes, and the XML declaration are preserved, but **mixed content** (text interleaved with child elements within the same parent — e.g. `<p>Hello <b>world</b>!</p>`) is **opaque**: the entire body is folded into the `"#text"` value and inner element positions are not tracked. Whitespace-only pretty-printing between elements is not preserved on round-trip; the writer emits a normalised compact-with-newlines layout. Both behaviours are documented as known limitations; mixed-content XML emits a `tracing::warn!` on parse so users are aware their file is partially round-trippable.

#### Scenario: Format trait registration
- **WHEN** a new struct `XmlFormat` is registered in `dq-core::format` and `format::detect(Utf8Path::new("pom.xml"))` is called
- **THEN** the result is `Some(&XmlFormat)`

#### Scenario: Element with attribute and text round-trips
- **GIVEN** an XML document `<user id="42"><name>Alice</name></user>`
- **WHEN** `XmlFormat::parse` is called and then `XmlFormat::write` is called on the result
- **THEN** the resulting bytes are functionally equivalent (semantically identical XML; whitespace between elements may differ)

#### Scenario: Multi-child same-tag preserves order
- **GIVEN** an XML document `<list><item>A</item><item>B</item><item>C</item></list>`
- **WHEN** `XmlFormat::parse` is called
- **THEN** the resulting `Value` has `/list/item` as an `Array` of three elements `["A", "B", "C"]` in that order, addressable as `/list/item/0`, `/list/item/1`, `/list/item/2`

#### Scenario: Comments preserved on round-trip
- **GIVEN** an XML document with a `<!-- top note -->` comment inside `<root>`
- **WHEN** `XmlFormat::parse` then `XmlFormat::write` runs
- **THEN** the output XML contains the same comment text in the same position relative to its sibling elements

#### Scenario: CDATA preserved on round-trip
- **GIVEN** an XML document with `<script><![CDATA[if (a < b) {}]]></script>`
- **WHEN** `XmlFormat::parse` then `XmlFormat::write` runs
- **THEN** the output XML contains the CDATA block with byte-identical content

#### Scenario: Mixed content emits warning
- **GIVEN** an XML document with `<p>Hello <b>world</b>!</p>`
- **WHEN** `XmlFormat::parse` is called
- **THEN** a `tracing::warn!` log is emitted noting that mixed content was encountered AND parsing succeeds with the body folded into `"#text"`

#### Scenario: XML declaration preserved
- **GIVEN** an XML document beginning with `<?xml version="1.0" encoding="UTF-8"?>`
- **WHEN** `XmlFormat::parse` then `XmlFormat::write` runs
- **THEN** the output XML begins with an equivalent declaration

#### Scenario: Auto-detection by extension
- **WHEN** the user runs `dq get pom.xml /project/version`
- **THEN** XML format is detected from the `.xml` extension and the value is returned

#### Scenario: `-F xml` override accepted on read
- **WHEN** the user runs `dq get config.txt -F xml /root/setting`
- **THEN** the file is parsed as XML regardless of the `.txt` extension
