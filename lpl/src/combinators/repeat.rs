use crate::{ParseStream, Parser};

pub fn one_or_more<'a, P, Input: ParseStream<'a> + 'a, Output>(
    parser: P,
) -> impl Parser<'a, Input, Vec<Output>>
where
    P: Parser<'a, Input, Output>,
{
    move |input| {
        let mut result = Vec::new();

        let mut next_input: Option<Input>;

        if let Ok((first_item, ni)) = parser.parse(input) {
            next_input = ni;
            result.push(first_item);
        } else {
            return Err("none found".to_string());
        }

        while let Some(ref inp) = next_input {
            if let Ok((next_item, ni)) = parser.parse(inp.clone()) {
                next_input = ni;
                result.push(next_item);
            } else {
                break;
            }
        }

        Ok((result, next_input))
    }
}

pub fn zero_or_more<'a, P, Input: ParseStream<'a> + 'a, Output>(
    parser: P,
) -> impl Parser<'a, Input, Vec<Output>>
where
    P: Parser<'a, Input, Output>,
{
    move |input| {
        let mut result = Vec::new();

        let mut next_input: Option<Input> = Some(input);

        while let Some(ref inp) = next_input {
            if let Ok((next_item, ni)) = parser.parse(inp.clone()) {
                next_input = ni;
                result.push(next_item);
            } else {
                break;
            }
        }

        Ok((result, next_input))
    }
}

pub fn interleaved<'a, P1, P2, Input: ParseStream<'a> + 'a, Output>(
    parser: P1,
    ignored: P2,
) -> impl Parser<'a, Input, Vec<Output>>
where
    P1: Parser<'a, Input, Output>,
    P2: Parser<'a, Input, ()>,
{
    move |input| {
        let mut result = Vec::new();

        let mut next_input: Option<Input> = Some(input);

        while let Some(ref inp) = next_input {
            if let Ok((_, ni)) = ignored.parse(inp.clone()) {
                next_input = ni;
                continue;
            }

            if let Ok((next_item, ni)) = parser.parse(inp.clone()) {
                next_input = ni;
                result.push(next_item);
            } else {
                break;
            }
        }

        Ok((result, next_input))
    }
}
