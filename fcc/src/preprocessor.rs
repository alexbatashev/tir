use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;

use logos::{Lexer, Logos};

use crate::lexer::Token;

// ---------------------------------------------------------------------------
// Preprocessor-directive token type
// ---------------------------------------------------------------------------

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t\r]+")]
enum PreprocToken {
    #[token("define")]     Define,
    #[token("undef")]      Undef,
    #[token("include")]    Include,
    #[token("ifdef")]      Ifdef,
    #[token("ifndef")]     Ifndef,
    #[token("if")]         If,
    #[token("elif")]       Elif,
    #[token("elifdef")]    Elifdef,
    #[token("elifndef")]   Elifndef,
    #[token("else")]       Else,
    #[token("endif")]      Endif,
    #[token("line")]       Line,
    #[token("error")]      Error,
    #[token("warning")]    Warning,
    #[token("pragma")]     Pragma,
    #[token("embed")]      Embed,

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Identifier,
    #[regex(r#""[^"]*""#)]
    QuotedPath,
    #[regex(r"<[^>]*>")]
    AnglePath,
}

// ---------------------------------------------------------------------------
// Conditional-compilation state machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum CondState {
    /// An outer level is already skipping — ignore everything at this level.
    OuterSkip,
    /// This branch is active: emit tokens.
    Active,
    /// No active branch seen yet at this level: skip tokens.
    Inactive,
    /// An active branch was already emitted: skip all remaining branches.
    Done,
}

fn is_skipping(stack: &[CondState]) -> bool {
    stack.iter().any(|s| !matches!(s, CondState::Active))
}

// ---------------------------------------------------------------------------
// TokenStream
// ---------------------------------------------------------------------------

/// Lazy preprocessed token stream.
///
/// Internally keeps a stack of `(source, byte_offset)` frames.  The top frame
/// is the one currently being lexed.  New frames are pushed for `#include`
/// files and object-macro expansions so that processing is interleaved rather
/// than collected up front.
///
/// Because `logos::Lexer<'s, T>` borrows `&'s str`, storing one in a struct
/// causes a self-referential lifetime problem.  We sidestep this by keeping
/// only `Rc<str>` + `usize` offset and reconstructing a short-lived lexer on
/// each call to `next()`.  Lexer initialisation is O(1) (pointer + state
/// setup), so this is negligible.
pub struct TokenStream {
    /// Stack of (owned source, current byte offset).  Top = active frame.
    source_stack: Vec<(Rc<str>, usize)>,
    defines: HashMap<String, String>,
    include_paths: Vec<PathBuf>,
    cond_stack: Vec<CondState>,
}

impl TokenStream {
    /// Set the top frame's offset to right after the next `\n` in `remainder`.
    ///
    /// `remainder` must be a suffix of the top frame's source.  The formula
    /// `source_len - remainder.len()` recovers the absolute position even
    /// though `remainder` was originally sliced from `source[offset..]`.
    fn skip_line(&mut self, source_len: usize, remainder: &str) {
        let new_offset = match remainder.find('\n') {
            Some(i) => source_len - remainder.len() + i + 1,
            None => source_len,
        };
        if let Some(frame) = self.source_stack.last_mut() {
            frame.1 = new_offset;
        }
    }

    fn process_directive(&mut self, source: &str, mut pp: Lexer<'_, PreprocToken>) {
        let skipping = is_skipping(&self.cond_stack);

        match pp.next() {
            Some(Ok(PreprocToken::Define)) if !skipping => {
                let name = match pp.next() {
                    Some(Ok(PreprocToken::Identifier)) => pp.slice().to_string(),
                    _ => { self.skip_line(source.len(), pp.remainder()); return; }
                };
                let remainder = pp.remainder();
                let body_end = remainder.find('\n').unwrap_or(remainder.len());
                let body = remainder[..body_end].trim().to_string();
                self.defines.insert(name, body);
                self.skip_line(source.len(), remainder);
            }

            Some(Ok(PreprocToken::Undef)) if !skipping => {
                if let Some(Ok(PreprocToken::Identifier)) = pp.next() {
                    self.defines.remove(pp.slice());
                }
                self.skip_line(source.len(), pp.remainder());
            }

            Some(Ok(PreprocToken::Include)) if !skipping => {
                let path = match pp.next() {
                    Some(Ok(PreprocToken::QuotedPath)) => {
                        let s = pp.slice(); s[1..s.len() - 1].to_string()
                    }
                    Some(Ok(PreprocToken::AnglePath)) => {
                        let s = pp.slice(); s[1..s.len() - 1].to_string()
                    }
                    _ => { self.skip_line(source.len(), pp.remainder()); return; }
                };
                self.skip_line(source.len(), pp.remainder());
                // Find and push the included file as a new lazy frame.
                let content = self.include_paths.iter()
                    .find_map(|dir| std::fs::read_to_string(dir.join(&path)).ok());
                if let Some(content) = content {
                    self.source_stack.push((Rc::from(content.as_str()), 0));
                }
            }

            Some(Ok(PreprocToken::Ifdef)) => {
                let name = match pp.next() {
                    Some(Ok(PreprocToken::Identifier)) => pp.slice().to_string(),
                    _ => String::new(),
                };
                let state = if skipping { CondState::OuterSkip }
                    else if self.defines.contains_key(&name) { CondState::Active }
                    else { CondState::Inactive };
                self.cond_stack.push(state);
                self.skip_line(source.len(), pp.remainder());
            }

            Some(Ok(PreprocToken::Ifndef)) => {
                let name = match pp.next() {
                    Some(Ok(PreprocToken::Identifier)) => pp.slice().to_string(),
                    _ => String::new(),
                };
                let state = if skipping { CondState::OuterSkip }
                    else if !self.defines.contains_key(&name) { CondState::Active }
                    else { CondState::Inactive };
                self.cond_stack.push(state);
                self.skip_line(source.len(), pp.remainder());
            }

            Some(Ok(PreprocToken::If)) => {
                // Expression evaluation not yet implemented; treated as false.
                let state = if skipping { CondState::OuterSkip } else { CondState::Inactive };
                self.cond_stack.push(state);
                self.skip_line(source.len(), pp.remainder());
            }

            Some(Ok(PreprocToken::Else)) => {
                if let Some(top) = self.cond_stack.last_mut() {
                    *top = match *top {
                        CondState::Inactive => CondState::Active,
                        CondState::Active   => CondState::Done,
                        other               => other,
                    };
                }
                self.skip_line(source.len(), pp.remainder());
            }

            Some(Ok(PreprocToken::Elif | PreprocToken::Elifdef | PreprocToken::Elifndef)) => {
                if let Some(top) = self.cond_stack.last_mut() {
                    *top = match *top {
                        CondState::Inactive => CondState::Active,
                        CondState::Active   => CondState::Done,
                        other               => other,
                    };
                }
                self.skip_line(source.len(), pp.remainder());
            }

            Some(Ok(PreprocToken::Endif)) => {
                self.cond_stack.pop();
                self.skip_line(source.len(), pp.remainder());
            }

            _ => self.skip_line(source.len(), pp.remainder()),
        }
    }
}

impl Iterator for TokenStream {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            // Drop exhausted frames.
            while self.source_stack.last().map_or(false, |(s, o)| *o >= s.len()) {
                self.source_stack.pop();
            }

            // Clone Rc (cheap) + copy offset so we release the shared borrow
            // on source_stack before taking &mut self below.
            let (source_rc, offset) = {
                let top = self.source_stack.last()?;
                (Rc::clone(&top.0), top.1)
            };

            let mut lexer = Token::lexer(&source_rc[offset..]);
            let tok = lexer.next();

            match tok {
                None => { self.source_stack.pop(); }

                Some(Err(_)) => {
                    // Unrecognised character — skip it.
                    let new = source_rc.len() - lexer.remainder().len();
                    self.source_stack.last_mut().unwrap().1 = new;
                }

                Some(Ok(Token::Hash)) => {
                    // morph hands the same source position to the directive lexer.
                    let pp = lexer.morph::<PreprocToken>();
                    self.process_directive(&source_rc, pp);
                    // process_directive always calls skip_line, which sets the offset.
                }

                Some(Ok(Token::Identifier)) => {
                    let name = lexer.slice().to_string();
                    let new = source_rc.len() - lexer.remainder().len();
                    self.source_stack.last_mut().unwrap().1 = new;
                    if !is_skipping(&self.cond_stack) {
                        if let Some(body) = self.defines.get(&name).cloned() {
                            // Push the macro body as a new frame to be lazily lexed.
                            if !body.is_empty() {
                                self.source_stack.push((Rc::from(body.as_str()), 0));
                            }
                            // Continue the loop — next iteration lexes the expansion.
                        } else {
                            return Some(Token::Identifier);
                        }
                    }
                }

                Some(Ok(c_tok)) => {
                    let new = source_rc.len() - lexer.remainder().len();
                    self.source_stack.last_mut().unwrap().1 = new;
                    if !is_skipping(&self.cond_stack) {
                        return Some(c_tok);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Preprocess C source code and return a lazy iterator over C tokens.
///
/// * `reader`        — source to preprocess
/// * `defines`       — predefined macros (name → replacement text)
/// * `include_paths` — directories searched for `#include` files
pub fn preprocessed(
    mut reader: impl Read,
    defines: HashMap<String, String>,
    include_paths: &[PathBuf],
) -> impl Iterator<Item = Token> {
    let mut source = String::new();
    reader.read_to_string(&mut source).unwrap_or_default();
    TokenStream {
        source_stack: vec![(Rc::from(source.as_str()), 0)],
        defines,
        include_paths: include_paths.to_vec(),
        cond_stack: Vec::new(),
    }
}
