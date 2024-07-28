use std::ops::{Bound, RangeBounds};

use lpl::{
    combinators::{any_whitespace1, interleaved, literal, text::take_while},
    ParseStream, Parser, StrStream,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    Ident(&'a str),
    // Reserved for future LSP work
    Whitespace,
    // Keywords
    Enum,
    Record,
    Reg,
    // Punctuation
    Colon,
    Semicolon,
    Comma,
    Eq,         // =
    Gt,         // >
    Lt,         // <
    Geq,        // >=
    Leq,        // <=
    BraceOpen,  // {
    BraceClose, // }
}

#[derive(Clone, Debug)]
pub struct TokenStream<'a> {
    tokens: &'a [Token<'a>],
}

impl<'a> TokenStream<'a> {
    pub fn new(tokens: &'a [Token<'a>]) -> Self {
        Self { tokens }
    }
}

impl<'a> ParseStream<'a> for TokenStream<'a> {
    type Slice = &'a [Token<'a>];

    fn get(&self, range: std::ops::Range<usize>) -> Option<Self::Slice> {
        let ub = match range.end_bound() {
            Bound::Included(value) => *value + 1,
            Bound::Excluded(value) => *value,
            Bound::Unbounded => self.tokens.len(),
        };

        if ub <= self.tokens.len() {
            Some(&self.tokens[range])
        } else {
            None
        }
    }

    fn slice(&self, range: std::ops::Range<usize>) -> Option<Self>
    where
        Self: Sized,
    {
        let ub = match range.end_bound() {
            Bound::Included(value) => *value + 1,
            Bound::Excluded(value) => *value,
            Bound::Unbounded => self.tokens.len(),
        };

        if ub <= self.tokens.len() {
            Some(Self {
                tokens: &self.tokens[range],
            })
        } else {
            None
        }
    }

    fn len(&self) -> usize {
        self.tokens.len()
    }

    fn span(&self) -> lpl::Span {
        todo!()
    }
}

fn keyword<'a>() -> impl Parser<'a, StrStream<'a>, Token<'a>> {
    literal("enum")
        .map(|_| Token::Enum)
        .or_else(literal("record").map(|_| Token::Record))
        .or_else(literal("reg").map(|_| Token::Reg))
}

fn ident<'a>() -> impl Parser<'a, StrStream<'a>, Token<'a>> {
    take_while(|c| c.is_alphanumeric() || *c == '_').map(|ident| Token::Ident(ident))
}

fn punct<'a>() -> impl Parser<'a, StrStream<'a>, Token<'a>> {
    literal("{")
        .map(|_| Token::BraceOpen)
        .or_else(literal("}").map(|_| Token::BraceClose))
        .or_else(literal(":").map(|_| Token::Colon))
        .or_else(literal(";").map(|_| Token::Semicolon))
        .or_else(literal(",").map(|_| Token::Comma))
        .or_else(literal("=").map(|_| Token::Eq))
        .or_else(literal(">").map(|_| Token::Gt))
        .or_else(literal("<").map(|_| Token::Lt))
}

pub fn tokenize<'a>(input: &'a str) -> Result<Vec<Token<'a>>, String> {
    let stream: StrStream = input.into();

    let token = keyword().or_else(ident()).or_else(punct());

    let tokenizer = interleaved(token, any_whitespace1());

    tokenizer.parse(stream).map(|(tokens, _)| tokens)
}

#[cfg(test)]
mod tests {
    use crate::{tokenize, Token};

    #[test]
    fn simple_tokenizer() {
        let input = "enum    InstructionSet {}";
        let res = tokenize(input);
        assert!(res.is_ok());
        let tokens = res.unwrap();
        assert_eq!(
            &tokens,
            &[
                Token::Enum,
                Token::Ident("InstructionSet"),
                Token::BraceOpen,
                Token::BraceClose
            ]
        );
        assert_eq!(tokens.len(), 4);
    }
}
