use std::{path::Path, sync::Arc};

use crate::dts::{ByteReader, HasSource, HasSpan, Position, Reader, Span};

#[derive(Eq, PartialEq, Debug, Clone)]
pub enum TokenKind {
    Ident(String),
    RValue(String),
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub struct HeaderToken {
    pub kind: TokenKind,
    pub span: Span,
    pub source: Arc<Path>,
}

impl HasSpan for HeaderToken {
    fn span(&self) -> Span {
        self.span
    }
}

impl HasSource for HeaderToken {
    fn source(&self) -> Arc<Path> {
        self.source.clone()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Line {
    bytes: Vec<u8>,
    end: Position,
}

pub(super) struct Lexer<R> {
    reader: R,
    source: Arc<Path>,
}

impl Lexer<ByteReader> {
    pub fn from_text(text: impl Into<String>, source: Arc<Path>) -> Self {
        Self {
            reader: ByteReader::from_string(text.into()),
            source,
        }
    }

    /// Returns next found directive identifier or None on EOF
    /// # as a standalone keyword tolerated
    pub fn next_directive(&mut self) -> Option<HeaderToken> {
        loop {
            self.skip_whitespace();
            if self.reader.peek() != Some(b'#') {
                self.read_till_newline()?;
                continue;
            }
            let start = self.reader.pos();
            self.reader.skip();
            let ident = self.read_while(Self::is_ident);
            if ident.is_empty() {
                self.read_till_newline()?;
                continue;
            }
            let token = HeaderToken {
                kind: TokenKind::Ident(String::from_utf8(ident).unwrap()),
                span: start.to(self.reader.pos()),
                source: self.source.clone(),
            };
            return Some(token);
        }
    }

    /// Returns the first identifier after a directive or none when the identifier is empty
    pub fn name(&mut self) -> Option<HeaderToken> {
        self.skip_whitespace();
        let start = self.reader.pos();
        let ident = self.read_while(Self::is_ident);
        if ident.is_empty() {
            return None;
        }

        Some(HeaderToken {
            kind: TokenKind::Ident(String::from_utf8(ident).unwrap()),
            span: start.to(self.reader.pos()),
            source: self.source.clone(),
        })
    }

    /// Returns the remaining directive contents until a terminating newline is found.
    pub fn rvalue(&mut self) -> Option<HeaderToken> {
        self.skip_whitespace();
        let start = self.reader.pos();
        let Line { bytes, end } = self.read_till_newline()?;
        // An empty rest-of-line is legal (a body-less `#define`, or an
        // argument-less directive like `#endif`) and yields an empty span;
        // `Position::to` asserts on equal positions, hence the special case.
        let span = if end > start {
            start.to(end)
        } else {
            Span::new(start, end)
        };

        Some(HeaderToken {
            kind: TokenKind::RValue(String::from_utf8(bytes).unwrap()),
            span,
            source: self.source.clone(),
        })
    }
}

impl Lexer<ByteReader> {
    fn read_while(&mut self, f: impl Fn(u8) -> bool) -> Vec<u8> {
        let mut vec = vec![];
        while let Some(c) = self.reader.peek() {
            if f(c) {
                vec.push(c);
                self.reader.skip();
            } else {
                break;
            }
        }
        vec
    }

    fn is_whitespace(c: u8) -> bool {
        c == b' ' || c == b'\t'
    }

    fn is_ident(c: u8) -> bool {
        c == b'_' || c.is_ascii_alphanumeric()
    }

    fn read_till_newline(&mut self) -> Option<Line> {
        let mut line = {
            let mut vec = vec![];
            let mut end;
            loop {
                end = self.reader.pos();
                let Some(c) = self.reader.consume() else {
                    if vec.is_empty() {
                        break None;
                    } else {
                        break Some((vec, end));
                    }
                };

                match c {
                    b'\n' => {
                        break Some((vec, end));
                    }
                    b'\\' => {
                        let mut skipped = vec![];
                        while let Some(c) = self.reader.peek() {
                            if Self::is_whitespace(c) {
                                skipped.push(c);
                                self.reader.skip();
                            } else {
                                break;
                            }
                        }
                        if let Some(b'\n') = self.reader.peek() {
                            self.reader.skip();
                        } else {
                            vec.push(c);
                            vec.append(&mut skipped);
                        }
                        continue;
                    }
                    b'/' if let Some(b'/') = self.reader.peek() => {
                        while self.reader.skip_if(|c| c != b'\n') {}
                        end = self.reader.pos();
                        self.reader.skip(); // \n
                        break Some((vec, end));
                    }
                    b'/' if let Some(b'*') = self.reader.peek() => {
                        self.reader.skip(); // *
                        loop {
                            match self.reader.consume() {
                                Some(b'*') if self.reader.peek() == Some(b'/') => {
                                    self.reader.skip();
                                    break;
                                }
                                Some(_) => {}
                                None => break,
                            }
                        }
                    }
                    c => {
                        vec.push(c);
                    }
                }
            }
        };

        if let Some((vec, _)) = line.as_mut() {
            // remove trailing whitespace
            // end not modified deliberately to highlight the whitespace as well
            while matches!(vec.last(), Some(b' ' | b'\t' | b'\r')) {
                vec.pop();
            }
        }

        return line.map(|(bytes, end)| Line { bytes, end });
    }

    fn skip_whitespace(&mut self) {
        while self.reader.skip_if(Self::is_whitespace) {}
    }
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use super::{Lexer, Line, TokenKind};
    use crate::dts::{ByteReader, Position};

    #[test]
    fn smoke() {
        let code = r#"
#define A 10
#define B (1<<10)
#define F(a) a
#include "h.h"
#include <h.h>
        "#;

        macro_rules! tokens {
            ($dir:literal, $value:literal) => {
                [
                    Some(TokenKind::Ident($dir.to_string())),
                    None,
                    Some(TokenKind::RValue($value.to_string())),
                ]
            };
            ($dir:literal, $name:literal, $value:literal) => {
                [
                    Some(TokenKind::Ident($dir.to_string())),
                    Some(TokenKind::Ident($name.to_string())),
                    Some(TokenKind::RValue($value.to_string())),
                ]
            };
        }

        let mut lexer = Lexer::from_text(code.to_string(), Path::new("a").into());

        macro_rules! assert_next_directive {
            ($dir:literal, $name: literal) => {
                assert_eq!(
                    [
                        lexer.next_directive().map(|dir| dir.kind),
                        lexer.name().map(|n| n.kind),
                        lexer.rvalue().map(|v| v.kind)
                    ],
                    tokens!($dir, $name)
                )
            };
            ($dir:literal, $name: literal, $value:literal) => {
                assert_eq!(
                    [
                        lexer.next_directive().map(|dir| dir.kind),
                        lexer.name().map(|n| n.kind),
                        lexer.rvalue().map(|v| v.kind)
                    ],
                    tokens!($dir, $name, $value)
                )
            };
        }

        assert_next_directive!("define", "A", "10");
        assert_next_directive!("define", "B", "(1<<10)");
        assert_next_directive!("define", "F", "(a) a");
        assert_next_directive!("include", "\"h.h\"");
        assert_next_directive!("include", "<h.h>");
    }

    #[test]
    fn collapser() {
        let input = r#"
A
B\
C D
// comment
E F\
G


Y Z
void main() {}
main("\n")
/**
 *

 */
        "#;
        let mut lex = Lexer {
            reader: ByteReader::from_string(input.to_string()),
            source: Path::new("a").into(),
        };

        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: vec![],
                end: Position::new(0, 0)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: vec![b'A'],
                end: Position::new(1, 1)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: b"BC D".to_vec(),
                end: Position::new(3, 3)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: vec![],
                end: Position::new(4, 10)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: b"E FG".to_vec(),
                end: Position::new(6, 1)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: vec![],
                end: Position::new(7, 0)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: vec![],
                end: Position::new(8, 0)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: b"Y Z".to_vec(),
                end: Position::new(9, 3)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: b"void main() {}".to_vec(),
                end: Position::new(10, 14)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: b"main(\"\\n\")".to_vec(),
                end: Position::new(11, 10)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: vec![],
                end: Position::new(15, 3)
            })
        );
        assert_eq!(
            lex.read_till_newline(),
            Some(Line {
                bytes: vec![],
                end: Position::new(16, 8)
            })
        );
        assert_eq!(lex.read_till_newline(), None)
    }
}
