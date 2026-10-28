use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

use crate::dts::HasSpan;

mod lexer;

use lexer::Lexer;
pub use lexer::{HeaderToken, TokenKind};

/// A `#define` in a header file.
///
/// The body is the raw replacement text — splices folded, comments stripped.
/// The goal of the LSP is to provide navigation/completion, not to reimplement c/cpp preprocessor.
#[derive(Eq, PartialEq, Debug, Clone)]
pub struct Define {
    pub name: HeaderToken,
    pub body: String,
    pub fn_like: bool,
}

pub fn resolve_value_str(defines: &BTreeMap<String, Define>, name: &str) -> Option<String> {
    let mut visited = BTreeSet::new();
    let mut current = name.to_string();

    loop {
        let mut body = current.trim();
        while let Some(inner) = strip_circumfix(body, '(', ')') {
            body = inner.trim();
        }
        if !visited.insert(body.to_string()) {
            return None;
        }
        if parse_number(body).is_some() {
            return Some(body.to_string());
        }
        if let Some(define) = defines.get(body) {
            if define.fn_like {
                return None;
            }
            current = define.body.clone();
        } else {
            return None;
        }
    }
}

fn parse_number(input: &str) -> Option<u64> {
    let input = input.trim_end_matches(|c| matches!(c, 'u' | 'U' | 'l' | 'L'));
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        input.parse().ok()
    }
}

#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub enum IncludeStyle {
    /// `#include "x.h"`
    Quoted,
    /// `#include <x.h>`
    Chevron,
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub struct Include {
    pub directive: HeaderToken,
    pub target: HeaderToken,
    pub path: String,
    pub style: IncludeStyle,
}

/// The result of scanning a header file.
///
/// Pure data: no I/O.
#[derive(Eq, PartialEq, Debug, Clone, Default)]
pub struct HeaderFile {
    pub includes: Vec<Include>,
    pub defines: Vec<Define>,
}

/// Strip a matching pair of delimiters, e.g. `<x.h>` → `x.h`.
/// Returns `None` if the text is not delimited by the given pair.
fn strip_circumfix(text: &str, open: char, close: char) -> Option<&str> {
    if !text.starts_with(open) || !text.ends_with(close) {
        return None;
    }
    text.get(open.len_utf8()..text.len() - close.len_utf8())
}

/// Scan a C header for `#define` and `#include` directives.
///
/// Unknown directives and non-directive lines are skipped silently — the
/// scanner is not a C diagnostics engine. An `#include` whose target is not
/// a plain quoted/chevron literal (e.g. `#include MACRO_HEADER`) cannot be
/// followed and is dropped.
pub fn parse_header(text: &str, source: Arc<Path>) -> HeaderFile {
    let mut includes = Vec::new();
    let mut defines = Vec::new();

    let mut lexer = Lexer::from_text(text, source);

    while let Some(directive) = lexer.next_directive() {
        let dir = if let TokenKind::Ident(dir) = &directive.kind {
            dir
        } else {
            unreachable!("next_directive only yields Ident tokens")
        };

        match dir.as_str() {
            "define" => {
                let Some(name) = lexer.name() else {
                    // `#define` with no macro name: nothing to record
                    let _ = lexer.rvalue();
                    continue;
                };
                let body_token = lexer.rvalue();
                let body = body_token
                    .as_ref()
                    .map(|rv| match &rv.kind {
                        TokenKind::RValue(body) => body.clone(),
                        _ => unreachable!("rvalue only yields RValue tokens"),
                    })
                    .unwrap_or_default();
                let adjacent = body_token
                    .as_ref()
                    .map(|rv| rv.span().start() == name.span().end())
                    .unwrap_or(false);
                let fn_like = body.starts_with('(') && adjacent;
                defines.push(Define {
                    name,
                    body,
                    fn_like,
                });
            }
            "include" => {
                // A missing target (`#include` at EOF) is not an input error;
                // there is simply nothing to record.
                let Some(target_token) = lexer.rvalue() else {
                    continue;
                };
                let TokenKind::RValue(target) = &target_token.kind else {
                    continue;
                };
                let (style, path) = if target.starts_with('<') {
                    (IncludeStyle::Chevron, strip_circumfix(target, '<', '>'))
                } else {
                    (IncludeStyle::Quoted, strip_circumfix(target, '"', '"'))
                };
                // End the borrow of `target` before moving `target_token`.
                let Some(path) = path.map(str::to_string) else {
                    continue;
                };
                includes.push(Include {
                    directive,
                    target: target_token,
                    path,
                    style,
                });
            }
            // Any other directive (`#ifndef`, `#if`, `#pragma`, line
            // markers, ...): consume the rest of its line and ignore it.
            _ => {
                let _ = lexer.rvalue();
            }
        }
    }

    HeaderFile { includes, defines }
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use super::*;
    use crate::dts::{Position, Span};

    fn source() -> Arc<Path> {
        Path::new("a.h").into()
    }

    fn ident(name: &str) -> TokenKind {
        TokenKind::Ident(name.to_string())
    }

    #[test]
    fn scan_assembles() {
        let code = r#"
#define A 10
#define F(a) a
#define ALIAS A
#define EMPTY
#include <dt-bindings/x.h>
#include "y.h"
#include MACRO
#pragma once
#endif
just some C code, not a directive
"#;
        let file = parse_header(code, source());

        assert_eq!(file.defines.len(), 4);
        assert_eq!(file.defines[0].name.kind, ident("A"));
        assert_eq!(file.defines[0].body, "10");
        assert!(!file.defines[0].fn_like);
        assert_eq!(file.defines[1].name.kind, ident("F"));
        assert_eq!(file.defines[1].body, "(a) a");
        assert!(file.defines[1].fn_like);
        assert_eq!(file.defines[2].name.kind, ident("ALIAS"));
        assert_eq!(file.defines[2].body, "A");
        assert!(!file.defines[2].fn_like);
        // `#define EMPTY`: the empty rest-of-line must not panic on an
        // empty span (`#endif` below exercises the same path).
        assert_eq!(file.defines[3].name.kind, ident("EMPTY"));
        assert_eq!(file.defines[3].body, "");

        assert_eq!(file.includes.len(), 2);
        assert_eq!(file.includes[0].path, "dt-bindings/x.h");
        assert_eq!(file.includes[0].style, IncludeStyle::Chevron);
        assert_eq!(file.includes[1].path, "y.h");
        assert_eq!(file.includes[1].style, IncludeStyle::Quoted);
    }

    #[test]
    fn define_spans_and_fn_like_adjacency() {
        let file = parse_header("#define F(a) a\n#define V 10\n", source());

        // function-like: the '(' is adjacent to the name
        assert_eq!(
            file.defines[0].name.span(),
            Span::new(Position::new(0, 8), Position::new(0, 9))
        );
        assert!(file.defines[0].fn_like);

        // object-like: whitespace between name and body ⇒ not fn-like
        assert_eq!(
            file.defines[1].name.span(),
            Span::new(Position::new(1, 8), Position::new(1, 9))
        );
        assert_eq!(file.defines[1].body, "10");
        assert!(!file.defines[1].fn_like);
    }

    #[test]
    fn include_tokens() {
        let file = parse_header("#include \"y.h\"", source());
        let include = &file.includes[0];
        // The directive token covers the whole `#include` keyword — the
        // span starts at the `#`.
        assert_eq!(
            include.directive.span(),
            Span::new(Position::new(0, 0), Position::new(0, 8))
        );
        assert_eq!(
            include.target.span(),
            Span::new(Position::new(0, 9), Position::new(0, 14))
        );
        assert_eq!(include.path, "y.h");
        assert_eq!(include.style, IncludeStyle::Quoted);
    }

    #[test]
    fn define_value_lookup() {
        macro_rules! btmap {
            [$(#define $name:literal $body:literal)+] => {
                {

                let mut defines = BTreeMap::new();
                $(
                    defines.insert($name.to_string(),
                        Define {
                            name: HeaderToken {
                                kind: TokenKind::Ident($name.to_string()),
                                span: Span::new(Position::zero(), Position::zero()),
                                source: Path::new("a").into(),
                            },
                            body: $body.to_string(),
                            fn_like: false,
                        },
                    );
                )+
                defines
                }
            };
        }

        let defines = btmap!(
            #define "A" "B"
            #define "B" "C"
            #define "C" "10"
        );

        let resolved = resolve_value_str(&defines, "A");

        assert_eq!(resolved, Some("10".to_string()));

        let defines = btmap!(
            #define "A" "B"
            #define "B" "C"
            #define "C" "A"
        );

        let resolved = resolve_value_str(&defines, "A");

        assert_eq!(resolved, None);

        let defines = btmap!(
            #define "A" "F(x) 3"
            #define "F(x)" "1"
        );

        let resolved = resolve_value_str(&defines, "A");

        assert_eq!(resolved, None);

        let defines = btmap!(
            #define "A" "B"
            #define "B" "C + 1"
        );

        let resolved = resolve_value_str(&defines, "B");

        assert_eq!(resolved, None);

        let defines = btmap!(
            #define "A" "B"
            #define "B" "(0x42UL)"
        );

        let resolved = resolve_value_str(&defines, "A");

        assert_eq!(resolved, Some("0x42UL".to_string()));
    }
}
