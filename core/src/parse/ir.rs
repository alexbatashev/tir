use std::any::Any;
use std::sync::Arc;

use crate::{Context, Error, Operation, Region};

use super::common::{Cursor, Span};
use super::text::Parser as TextParser;

pub fn parse_ir<'a, T: Operation>(context: &Context, src: &'a str) -> Result<T, (Span, Error)> {
    let mut parser = TextParser::new(src);

    let op = parse_single_op(&mut parser, context)?;
    let any: Box<dyn Any> = op.into_any();
    any.downcast::<T>()
        .map(|t| *t)
        .map_err(|_| (Span(0), Error::ExpectedOperation(T::dialect(), T::name())))
}

fn parse_single_op<'src>(
    parser: &mut TextParser<'src>,
    context: &Context,
) -> Result<Box<dyn Operation>, (Span, Error)> {
    parser.skip_trivia();
    if let Some(name) = parser.parse_ident() {
        let (dialect, name) = if parser.peek_char() == Some('.') {
            if let Some(op_name) = parser.parse_ident() {
                (name, op_name)
            } else {
                return Err((parser.span(), Error::ExpectedOpName));
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

impl<'src> TextParser<'src> {
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
}
