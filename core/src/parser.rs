use std::{any::Any, sync::Arc};

use crate::{Context, Error, Operation, Region};

pub struct IRParser<'src> {
    src: &'src str,
    position: u32,
}

#[derive(Debug, Copy, Clone)]
pub struct Span(u32);

pub fn parse_ir<'a, T: Operation>(context: &Context, src: &'a str) -> Result<T, (Span, Error)> {
    let mut parser = IRParser::new(src);

    let op = parse_single_op(&mut parser, context)?;
    let any: Box<dyn Any> = op.into_any();
    any.downcast::<T>()
        .map(|t| *t)
        .map_err(|_| (Span(0), Error::ExpectedOperation(T::dialect(), T::name())))
}

fn parse_single_op<'src>(
    parser: &mut IRParser<'src>,
    context: &Context,
) -> Result<Box<dyn Operation>, (Span, Error)> {
    parser.skip_trivia();
    if let Some(name) = parser.parse_ident() {
        let (dialect, name) = if parser.peek() == '.' {
            if let Some(op_name) = parser.parse_ident() {
                (name, op_name)
            } else {
                todo!()
            }
        } else {
            ("builtin", name)
        };

        parser.skip_trivia();
        let op_parser = context
            .get_parser(dialect, name)
            .map_err(|e| (parser.span(), e))?;

        op_parser(parser, context)
    } else {
        Err((parser.span(), Error::ExpectedOpName))
    }
}

impl<'src> IRParser<'src> {
    pub fn new(src: &'src str) -> Self {
        Self { src, position: 0 }
    }

    pub fn span(&self) -> Span {
        Span(self.position)
    }

    pub fn peek(&self) -> char {
        self.src.chars().nth(self.position as usize).unwrap()
    }

    pub fn parse_ident(&mut self) -> Option<&'src str> {
        let start = self.position as usize;

        if !self.src.chars().nth(start).unwrap().is_alphabetic() {
            None
        } else {
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
        }
    }

    pub fn parse_token(&mut self, token: &str) -> bool {
        if self.src[self.position as usize..].starts_with(token) {
            self.position += token.len() as u32;
            self.skip_trivia();
            true
        } else {
            false
        }
    }

    pub fn parse_single_block_region(
        &mut self,
        context: &Context,
    ) -> Result<Arc<Region>, (Span, Error)> {
        if !self.parse_token("{") {
            return Err((self.span(), Error::ExpectedToken("{")));
        }

        let mut ops = vec![];

        // FIXME: this is not very error resilient
        while let Ok(op) = parse_single_op(self, context) {
            eprintln!("PARSED OP WITH ID {:?}", op.id());
            ops.push(op.id());
        }

        if !self.parse_token("}") {
            return Err((self.span(), Error::ExpectedToken("}")));
        }

        let region = context.create_region();
        let block = context.create_block(vec![]);
        region.add_block(block.id());
        for (idx, id) in ops.iter().enumerate() {
            block.insert(idx, *id);
        }

        Ok(region)
    }

    pub fn skip_trivia(&mut self) {
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
