use crate::parse::common::{Cursor, Span};

pub struct Parser<'src> {
    src: &'src str,
    position: u32,
}

impl<'src> Parser<'src> {
    pub fn new(src: &'src str) -> Self {
        Self { src, position: 0 }
    }

    pub fn peek_char(&self) -> Option<char> {
        self.src.chars().nth(self.position as usize)
    }

    pub fn parse_ident(&mut self) -> Option<&'src str> {
        let start = self.position as usize;

        if self
            .src
            .chars()
            .nth(start)
            .map(|c| c.is_alphabetic())
            .unwrap_or(false)
        {
            let mut last = start + 1;
            while let Some(c) = self.src.chars().nth(last) {
                if !c.is_alphanumeric() && c != '_' {
                    break;
                }
                last += 1;
            }

            self.position = last as u32;
            self.skip_trivia();
            Some(&self.src[start..last])
        } else {
            None
        }
    }

    pub fn parse_token(&mut self, token: &str) -> bool {
        if self
            .src
            .get(self.position as usize..)
            .map(|s| s.starts_with(token))
            .unwrap_or(false)
        {
            self.position += token.len() as u32;
            self.skip_trivia();
            true
        } else {
            false
        }
    }
}

impl Cursor for Parser<'_> {
    fn span(&self) -> Span {
        Span(self.position)
    }

    fn skip_trivia(&mut self) {
        let mut last = self.position as usize;
        while let Some(c) = self.src.chars().nth(last) {
            if !c.is_whitespace() && c != '\n' {
                break;
            }
            last += 1;
        }

        self.position = last as u32;
    }
}
