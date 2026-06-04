//! Parsing and compilation of FileCheck patterns.
//!
//! A pattern is the text following a `CHECK[-SUFFIX]:` directive. It is a
//! sequence of fixed literal text, regular-expression blocks `{{...}}`, variable
//! definitions `[[NAME:regex]]` and variable uses `[[NAME]]`. This mirrors the
//! syntax implemented by LLVM's FileCheck (see
//! `llvm/lib/FileCheck/FileCheck.cpp`), trimmed down to the features the TIR
//! test-suite relies on.
//!
//! The grammar is parsed with [`chumsky`]; the resulting segments are later
//! compiled into a [`regex::Regex`] taking the current variable bindings into
//! account.

use std::collections::HashMap;

use chumsky::prelude::*;

/// A single piece of a compiled pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// Fixed text that must appear verbatim (modulo whitespace canonicalisation).
    Literal(String),
    /// A regular expression, written `{{...}}` in the source.
    Regex(String),
    /// A variable definition `[[NAME:regex]]`. Captures the matched text under
    /// `name` for later use.
    VarDef { name: String, regex: String },
    /// A variable use `[[NAME]]`. Substitutes the previously captured value.
    VarRef(String),
}

/// A parsed (but not yet compiled) pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub segments: Vec<Segment>,
    /// The original pattern text, kept for diagnostics.
    pub raw: String,
}

/// Error produced while compiling a pattern into a regex.
#[derive(Debug, Clone)]
pub enum PatternError {
    /// The pattern text could not be parsed.
    Parse(String),
    /// A referenced variable was never defined.
    UndefinedVariable(String),
    /// The generated regex was rejected by the regex engine.
    Regex(String),
}

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatternError::Parse(s) => write!(f, "invalid pattern: {s}"),
            PatternError::UndefinedVariable(n) => write!(f, "undefined variable '{n}'"),
            PatternError::Regex(s) => write!(f, "invalid regular expression: {s}"),
        }
    }
}

fn parser<'src>() -> impl Parser<'src, &'src str, Vec<Segment>, extra::Err<Cheap>> {
    // `{{ regex }}` -- everything up to the closing `}}`.
    let regex_block = just("{{")
        .ignore_then(any().and_is(just("}}").not()).repeated().to_slice())
        .then_ignore(just("}}"))
        .map(|s: &str| Segment::Regex(s.to_string()));

    // The body of a `[[ ... ]]` block: an identifier optionally followed by
    // `:regex`.
    let ident = any()
        .filter(|c: &char| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '$' || *c == '@')
        .repeated()
        .at_least(1)
        .to_slice();

    let var_def = ident
        .then_ignore(just(':'))
        .then(any().and_is(just("]]").not()).repeated().to_slice())
        .map(|(name, regex): (&str, &str)| Segment::VarDef {
            name: name.to_string(),
            regex: regex.to_string(),
        });

    let var_ref = ident.map(|name: &str| Segment::VarRef(name.to_string()));

    let var_block = just("[[")
        .ignore_then(var_def.or(var_ref))
        .then_ignore(just("]]"));

    // Literal text: anything that does not start a `{{` or `[[` block.
    let literal = any()
        .and_is(just("{{").not())
        .and_is(just("[[").not())
        .repeated()
        .at_least(1)
        .to_slice()
        .map(|s: &str| Segment::Literal(s.to_string()));

    choice((regex_block, var_block, literal))
        .repeated()
        .collect::<Vec<_>>()
}

impl Pattern {
    /// Parse a pattern from the raw directive text.
    pub fn parse(raw: &str) -> Result<Pattern, PatternError> {
        let result = parser().parse(raw);
        if result.has_errors() {
            return Err(PatternError::Parse(raw.to_string()));
        }
        let segments = result.into_output().unwrap_or_default();
        Ok(Pattern {
            segments,
            raw: raw.to_string(),
        })
    }

    /// Returns true if the pattern is purely literal text (no regex/variables).
    pub fn is_plain(&self) -> bool {
        self.segments
            .iter()
            .all(|s| matches!(s, Segment::Literal(_)))
    }

    /// Compile the pattern to a regex given the current variable bindings.
    ///
    /// `strict_ws` keeps whitespace verbatim; otherwise runs of horizontal
    /// whitespace in literal text match one-or-more spaces/tabs, matching the
    /// default FileCheck behaviour. `full_line` anchors the match to a whole
    /// line.
    pub fn compile(
        &self,
        vars: &HashMap<String, String>,
        strict_ws: bool,
        full_line: bool,
    ) -> Result<CompiledPattern, PatternError> {
        let mut source = String::new();
        // Map of regex capture-group name -> FileCheck variable name.
        let mut captures: Vec<(String, String)> = Vec::new();

        if full_line {
            source.push_str("(?m)^");
        }

        for seg in &self.segments {
            match seg {
                Segment::Literal(text) => {
                    source.push_str(&compile_literal(text, strict_ws));
                }
                Segment::Regex(re) => {
                    source.push_str("(?:");
                    source.push_str(re);
                    source.push(')');
                }
                Segment::VarRef(name) => {
                    let value = vars
                        .get(name)
                        .ok_or_else(|| PatternError::UndefinedVariable(name.clone()))?;
                    source.push_str(&regex::escape(value));
                }
                Segment::VarDef { name, regex } => {
                    let group = sanitize_group_name(name, captures.len());
                    source.push_str("(?P<");
                    source.push_str(&group);
                    source.push('>');
                    source.push_str(regex);
                    source.push(')');
                    captures.push((group, name.clone()));
                }
            }
        }

        if full_line {
            source.push('$');
        }

        let regex = regex::Regex::new(&source).map_err(|e| PatternError::Regex(e.to_string()))?;
        Ok(CompiledPattern { regex, captures })
    }
}

/// A pattern compiled against a concrete set of variable bindings.
pub struct CompiledPattern {
    pub regex: regex::Regex,
    /// (regex group name, FileCheck variable name) for each captured variable.
    pub captures: Vec<(String, String)>,
}

impl CompiledPattern {
    /// Extract the variables defined by this pattern from a successful match.
    pub fn extract_vars(&self, caps: &regex::Captures) -> Vec<(String, String)> {
        self.captures
            .iter()
            .filter_map(|(group, var)| {
                caps.name(group)
                    .map(|m| (var.clone(), m.as_str().to_string()))
            })
            .collect()
    }
}

fn compile_literal(text: &str, strict_ws: bool) -> String {
    if strict_ws {
        return regex::escape(text);
    }

    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            while chars.peek().is_some_and(|n| n.is_whitespace()) {
                chars.next();
            }
            out.push_str("[ \\t]+");
        } else {
            out.push_str(&regex::escape(&c.to_string()));
        }
    }
    out
}

fn sanitize_group_name(name: &str, index: usize) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Regex group names must start with a letter or underscore.
    format!("v{index}_{sanitized}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_literal() {
        let p = Pattern::parse("hello world").unwrap();
        assert_eq!(p.segments, vec![Segment::Literal("hello world".into())]);
        assert!(p.is_plain());
    }

    #[test]
    fn parses_regex_block() {
        let p = Pattern::parse("foo {{.*}} bar").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Literal("foo ".into()),
                Segment::Regex(".*".into()),
                Segment::Literal(" bar".into()),
            ]
        );
    }

    #[test]
    fn parses_var_def_and_ref() {
        let p = Pattern::parse("%[[REG:r[0-9]+]] = [[REG]]").unwrap();
        assert_eq!(
            p.segments,
            vec![
                Segment::Literal("%".into()),
                Segment::VarDef {
                    name: "REG".into(),
                    regex: "r[0-9]+".into()
                },
                Segment::Literal(" = ".into()),
                Segment::VarRef("REG".into()),
            ]
        );
    }

    #[test]
    fn whitespace_is_canonicalised() {
        let p = Pattern::parse("add  x3").unwrap();
        let c = p.compile(&HashMap::new(), false, false).unwrap();
        assert!(c.regex.is_match("add x3"));
        assert!(c.regex.is_match("add      x3"));
    }

    #[test]
    fn var_round_trip() {
        let p = Pattern::parse("[[X:[0-9]+]]").unwrap();
        let c = p.compile(&HashMap::new(), false, false).unwrap();
        let caps = c.regex.captures("42").unwrap();
        let vars = c.extract_vars(&caps);
        assert_eq!(vars, vec![("X".to_string(), "42".to_string())]);
    }
}
