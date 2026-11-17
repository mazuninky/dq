//! Dockerfile / Containerfile read-only parser (via `dockerfile-parser-rs`).
//!
//! Top-level shape: `Array<Map { instruction: String, arguments: String|Array,
//! line: Int }>`. The parser produces a flat list of instruction records
//! preserving source order. `Comment` and `Empty` AST nodes are skipped —
//! the value tree is the structural instructions only.
//!
//! Per design D6, this format is **read-only**: `Format::write` always
//! returns `Error::Format { format: "dockerfile", message: contains
//! "read-only" }`. `OutputFormat::Dockerfile` does not exist on the CLI side
//! so `dq convert -F dockerfile` is rejected at the clap layer.
//!
//! ## Line numbers
//!
//! `dockerfile-parser-rs` 3.3 does NOT expose per-instruction line numbers
//! through its public AST. The `line` field on each emitted map carries the
//! 1-based instruction index (i.e. the position in the parsed list) as a
//! pragmatic substitute. Tests in Stage 4 will pin this contract; if the
//! upstream crate later surfaces real line info, the field can switch
//! transparently.

use std::collections::BTreeMap;
use std::io::Write;

use dockerfile_parser_rs::{Dockerfile as ParsedDockerfile, Instruction};
use indexmap::IndexMap;

use crate::Result;
use crate::document::{Document, FormatTag, Value};
use crate::error::Error;
use crate::format::Format;

/// Dockerfile format implementation (read-only).
#[derive(Debug, Clone, Copy)]
pub struct Dockerfile;

impl Format for Dockerfile {
    fn name(&self) -> &'static str {
        "dockerfile"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["dockerfile", "containerfile"]
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("invalid UTF-8 in Dockerfile input: {e}"),
        })?;
        let parsed: ParsedDockerfile = text.parse().map_err(|e| Error::Parse {
            file: None,
            line: 0,
            col: 0,
            span: 0..0,
            snippet: String::new(),
            message: format!("Dockerfile parse error: {e}"),
        })?;

        let mut rows: Vec<Value> = Vec::new();
        let mut emitted_index: i64 = 0;
        for instr in parsed.instructions {
            let Some((name, args)) = render_instruction(&instr) else {
                continue;
            };
            emitted_index += 1;
            let mut row: IndexMap<String, Value> = IndexMap::new();
            row.insert("instruction".into(), Value::String(name.to_owned()));
            row.insert("arguments".into(), args);
            // No real line numbers available from the upstream crate; expose
            // the 1-based instruction index as a deterministic substitute.
            row.insert("line".into(), Value::Int(emitted_index));
            rows.push(Value::Map(row));
        }
        Ok(Document::value_only(
            Value::Array(rows),
            FormatTag::Dockerfile,
        ))
    }

    fn write(&self, _doc: &Document, _w: &mut dyn Write) -> Result<()> {
        Err(Error::Format {
            format: "dockerfile",
            message: "Dockerfile is read-only in M5".to_owned(),
        })
    }
}

/// Map a parsed `Instruction` into its emitted `(name, arguments)` pair.
///
/// Returns `None` for ignored AST nodes (comments, empty lines). Multi-token
/// args produce `Value::Array<String>`; single-string args (`FROM image`,
/// `WORKDIR path`) produce `Value::String`. Map-shaped args (`ARG`, `ENV`,
/// `LABEL`) produce `Value::Map<String, String>`.
fn render_instruction(instr: &Instruction) -> Option<(&'static str, Value)> {
    match instr {
        Instruction::Comment(_) | Instruction::Empty {} => None,
        Instruction::From { image, alias, .. } => {
            let mut s = image.clone();
            if let Some(a) = alias {
                s.push_str(" AS ");
                s.push_str(a);
            }
            Some(("FROM", Value::String(s)))
        }
        Instruction::Run {
            command, heredoc, ..
        } => {
            let mut combined = command.clone();
            if let Some(hd) = heredoc {
                combined.extend(hd.iter().cloned());
            }
            Some(("RUN", string_or_array(combined)))
        }
        Instruction::Cmd(items) => Some(("CMD", string_or_array(items.clone()))),
        Instruction::Entrypoint(items) => Some(("ENTRYPOINT", string_or_array(items.clone()))),
        Instruction::Shell(items) => Some(("SHELL", string_or_array(items.clone()))),
        Instruction::Copy {
            sources,
            destination,
            ..
        } => {
            let mut all: Vec<String> = sources.clone();
            all.push(destination.clone());
            Some(("COPY", string_or_array(all)))
        }
        Instruction::Add {
            sources,
            destination,
            ..
        } => {
            let mut all: Vec<String> = sources.clone();
            all.push(destination.clone());
            Some(("ADD", string_or_array(all)))
        }
        Instruction::Workdir { path } => Some(("WORKDIR", Value::String(path.clone()))),
        Instruction::User { user, group } => {
            let mut s = user.clone();
            if let Some(g) = group {
                s.push(':');
                s.push_str(g);
            }
            Some(("USER", Value::String(s)))
        }
        Instruction::Stopsignal { signal } => Some(("STOPSIGNAL", Value::String(signal.clone()))),
        Instruction::Expose { ports } => Some(("EXPOSE", string_or_array(ports.clone()))),
        Instruction::Volume { mounts } => Some(("VOLUME", string_or_array(mounts.clone()))),
        Instruction::Env(map) => Some(("ENV", btree_map_to_value(map))),
        Instruction::Label(map) => Some(("LABEL", btree_map_to_value(map))),
        Instruction::Arg(map) => Some(("ARG", arg_btree_map_to_value(map))),
    }
}

fn string_or_array(items: Vec<String>) -> Value {
    if items.len() == 1 {
        Value::String(items.into_iter().next().expect("len==1"))
    } else {
        Value::Array(items.into_iter().map(Value::String).collect())
    }
}

fn btree_map_to_value(map: &BTreeMap<String, String>) -> Value {
    let mut out: IndexMap<String, Value> = IndexMap::with_capacity(map.len());
    for (k, v) in map {
        out.insert(k.clone(), Value::String(v.clone()));
    }
    Value::Map(out)
}

fn arg_btree_map_to_value(map: &BTreeMap<String, Option<String>>) -> Value {
    let mut out: IndexMap<String, Value> = IndexMap::with_capacity(map.len());
    for (k, v) in map {
        let v_val = v
            .as_ref()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null);
        out.insert(k.clone(), v_val);
    }
    Value::Map(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_dockerfile() {
        let doc = Dockerfile.parse(b"FROM alpine\n").expect("parse");
        let Value::Array(items) = doc.value() else {
            panic!("expected array")
        };
        assert_eq!(items.len(), 1);
        let Value::Map(row) = &items[0] else {
            panic!("expected row map")
        };
        assert_eq!(row.get("instruction"), Some(&Value::String("FROM".into())));
        assert_eq!(row.get("arguments"), Some(&Value::String("alpine".into())));
    }

    #[test]
    fn write_returns_format_error() {
        let doc = Document::value_only(Value::Array(vec![]), FormatTag::Dockerfile);
        let mut buf: Vec<u8> = Vec::new();
        let err = Dockerfile.write(&doc, &mut buf).expect_err("read-only");
        match err {
            Error::Format { format, message } => {
                assert_eq!(format, "dockerfile");
                assert!(
                    message.contains("read-only"),
                    "expected read-only message; got: {message}",
                );
            }
            other => panic!("expected Format error, got {other:?}"),
        }
    }
}
