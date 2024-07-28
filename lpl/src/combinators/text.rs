use crate::{parse_stream::ParseStream, ParseResult, Parser};

pub fn any_char<'a, Input>(input: Input) -> ParseResult<Input, char>
where
    Input: ParseStream<'a>,
{
    if !input.is_string_like() {
        return Err("Expected string-like input".to_string());
    }

    match input.chars().next() {
        Some(next) => {
            let next_input: Option<Input> = input.slice(next.len_utf8()..input.len());
            Ok((next, next_input))
        }
        _ => Err("Expected a char, got end of string".to_string()),
    }
}

pub fn take_while<'a, Input, F>(predicate: F) -> impl Parser<'a, Input, &'a str>
where
    Input: ParseStream<'a> + 'a,
    F: Fn(&char) -> bool,
{
    move |input: Input| {
        if !input.is_string_like() {
            return Err("Expected string-like input".to_string());
        }

        let mut last = 0;

        let chars = input.chars();

        for c in chars {
            if !predicate(&c) {
                break;
            }
            last += c.len_utf8();
        }

        if last == 0 {
            return Err("".to_string());
        }

        let next_input: Option<Input> = input.slice(last..input.len());

        let substr = input.substr(0..last).unwrap();

        Ok((substr, next_input))
    }
}
