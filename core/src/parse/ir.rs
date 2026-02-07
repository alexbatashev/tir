use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use crate::value::ValueId;
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

pub(crate) fn parse_single_op<'src>(
    parser: &mut TextParser<'src>,
    context: &Context,
) -> Result<Box<dyn Operation>, (Span, Error)> {
    parser.skip_trivia();

    // Optional SSA result assignment prefix (e.g. "%2 =").
    // The concrete ValueId is currently allocated by builders from context state.
    let mark = parser.pos();
    if parser.parse_value_ref().is_some() && !parser.parse_token("=") {
        parser.set_pos(mark);
    }

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

/// Maps value names (e.g. "0", "1", "arg") to ValueIds during parsing.
#[derive(Default, Clone)]
pub struct ValueScope {
    values: HashMap<String, ValueId>,
}

impl ValueScope {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: String, id: ValueId) {
        self.values.insert(name, id);
    }

    pub fn get(&self, name: &str) -> Option<ValueId> {
        self.values.get(name).copied()
    }
}

impl<'src> TextParser<'src> {
    pub fn parse_single_block_region(
        &mut self,
        context: &Context,
    ) -> Result<Arc<Region>, (Span, Error)> {
        self.parse_single_block_region_with_args(context, vec![])
    }

    pub fn parse_single_block_region_with_args(
        &mut self,
        context: &Context,
        block_args: Vec<crate::Value>,
    ) -> Result<Arc<Region>, (Span, Error)> {
        if !self.parse_token("{") {
            return Err((self.span(), Error::ExpectedToken("{")));
        }

        let mut ops = vec![];

        // FIXME: this is not very error resilient
        while let Ok(op) = parse_single_op(self, context) {
            ops.push(op.id());
        }

        if !self.parse_token("}") {
            return Err((self.span(), Error::ExpectedToken("}")));
        }

        let region = context.create_region();
        let block = context.create_block(block_args);
        region.add_block(block.id());
        for (idx, id) in ops.iter().enumerate() {
            block.insert(idx, *id);
        }

        Ok(region)
    }
}
