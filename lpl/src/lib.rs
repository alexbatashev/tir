pub mod combinators;

mod parse_stream;

pub use parse_stream::*;

use std::sync::Arc;

pub struct Span {
    filename: Option<Arc<String>>,
    offset: usize,
}

impl Span {
    pub fn new(filename: Option<Arc<String>>, offset: usize) -> Self {
        Self { filename, offset }
    }

    pub fn get_filename<'a>(&'a self) -> Option<&'a str> {
        match &self.filename {
            Some(filename) => Some(filename.as_ref()),
            None => None,
        }
    }

    pub fn get_offset(&self) -> usize {
        self.offset
    }
}

pub type Spanned<Type> = (Type, Span);

pub type ParseResult<Input, Output> = Result<(Spanned<Output>, Option<Input>), String>;

pub trait Parser<'a, Input: ParseStream<'a> + 'a, Output> {
    fn parse(&self, input: Input) -> ParseResult<Input, Output>;

    fn map<F, NewOutput>(self, map_fn: F) -> BoxedParser<'a, Input, NewOutput>
    where
        Self: Sized + 'a,
        Output: 'a,
        NewOutput: 'a,
        F: Fn(Output) -> NewOutput + 'a,
    {
        BoxedParser::new(combinators::map(self, map_fn))
    }

    fn or_else<P2>(self, parser2: P2) -> BoxedParser<'a, Input, Output>
    where
        P2: Parser<'a, Input, Output> + 'a,
        Self: Sized + 'a,
        Output: 'a,
    {
        BoxedParser::new(combinators::or_else(self, parser2))
    }

    fn and_then<P2, Output2>(self, parser2: P2) -> BoxedParser<'a, Input, (Output, Output2)>
    where
        Output: 'a,
        Output2: 'a,
        Self: Sized + 'a,
        P2: Parser<'a, Input, Output2> + 'a,
    {
        BoxedParser::new(combinators::and_then(self, parser2))
    }
}

impl<'a, F, Input, Output> Parser<'a, Input, Output> for F
where
    F: Fn(Input) -> ParseResult<Input, Output>,
    Input: ParseStream<'a> + 'a,
{
    fn parse(&self, input: Input) -> ParseResult<Input, Output> {
        self(input)
    }
}

pub struct BoxedParser<'a, Input, Output>
where
    Input: ParseStream<'a>,
{
    parser: Box<dyn Parser<'a, Input, Output> + 'a>,
}

impl<'a, Input: ParseStream<'a>, Output> BoxedParser<'a, Input, Output> {
    pub fn new<P>(parser: P) -> Self
    where
        P: Parser<'a, Input, Output> + 'a,
    {
        BoxedParser {
            parser: Box::new(parser),
        }
    }
}

impl<'a, Input: ParseStream<'a>, Output> Parser<'a, Input, Output>
    for BoxedParser<'a, Input, Output>
{
    fn parse(&self, input: Input) -> ParseResult<Input, Output> {
        self.parser.parse(input)
    }
}
