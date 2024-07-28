use crate::{ParseStream, Parser};

pub fn map<'a, P, F, Input, Output1, Output2>(
    parser: P,
    map_fn: F,
) -> impl Parser<'a, Input, Output2>
where
    Input: ParseStream<'a> + 'a,
    P: Parser<'a, Input, Output1>,
    F: Fn(Output1) -> Output2,
{
    move |input: Input| {
        parser
            .parse(input)
            .map(|(result, next_input)| ((map_fn(result.0), result.1), next_input))
    }
}

pub fn or_else<'a, P1, P2, Input, Output>(
    parser1: P1,
    parser2: P2,
) -> impl Parser<'a, Input, Output>
where
    Input: ParseStream<'a> + 'a,
    P1: Parser<'a, Input, Output>,
    P2: Parser<'a, Input, Output>,
{
    move |input: Input| {
        parser1
            .parse(input.clone())
            .or_else(|_| parser2.parse(input))
    }
}

pub fn and_then<'a, P1, P2, Input, Output1, Output2>(
    parser1: P1,
    parser2: P2,
) -> impl Parser<'a, Input, (Output1, Output2)>
where
    Input: ParseStream<'a> + 'a,
    P1: Parser<'a, Input, Output1>,
    P2: Parser<'a, Input, Output2>,
{
    move |input: Input| {
        parser1
            .parse(input.clone())
            .and_then(|(out, next_input)| match next_input {
                Some(next_input) => parser2
                    .parse(next_input)
                    .map(|(out2, next_input)| ((out, out2), next_input)),
                None => Err("no more input to parse".to_string()),
            })
    }
}
