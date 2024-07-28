use crate::combinators::pred;
use crate::ParseStream;
use crate::Parser;

use super::one_or_more;
use super::text::any_char;
use super::zero_or_more;

pub fn any_whitespace<'a, Input>() -> impl Parser<'a, Input, char>
where
    Input: ParseStream<'a> + 'a,
{
    pred(any_char, |c| c.is_whitespace())
}

pub fn any_whitespace0<'a, Input>() -> impl Parser<'a, Input, ()>
where
    Input: ParseStream<'a> + 'a,
{
    zero_or_more(any_whitespace()).map(|_| ())
}

pub fn any_whitespace1<'a, Input>() -> impl Parser<'a, Input, ()>
where
    Input: ParseStream<'a> + 'a,
{
    one_or_more(any_whitespace()).map(|_| ())
}

pub fn space<'a, Input>() -> impl Parser<'a, Input, char>
where
    Input: ParseStream<'a> + 'a,
{
    pred(any_char, |c| *c == ' ' || *c == '\t')
}

pub fn space0<'a, Input>() -> impl Parser<'a, Input, ()>
where
    Input: ParseStream<'a> + 'a,
{
    zero_or_more(space()).map(|_| ())
}

pub fn space1<'a, Input>() -> impl Parser<'a, Input, ()>
where
    Input: ParseStream<'a> + 'a,
{
    one_or_more(space()).map(|_| ())
}
