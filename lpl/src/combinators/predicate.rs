use crate::{parse_stream::ParseStream, Parser};

pub fn pred<'a, Input, Output, P, F>(parser: P, predicate: F) -> impl Parser<'a, Input, Output>
where
    Input: ParseStream<'a> + 'a,
    F: Fn(&Output) -> bool,
    P: Parser<'a, Input, Output>,
{
    move |input| match parser.parse(input) {
        Ok((value, next_input)) => {
            if predicate(&value) {
                Ok((value, next_input))
            } else {
                Err("TODO error message".to_string())
            }
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use crate::combinators::{pred, text::any_char};
    use crate::parse_stream::StrStream;
    use crate::Parser;

    #[test]
    fn predicate_combinator() {
        let input: StrStream = "Hello World".into();
        let parser = pred(any_char, |c| *c == 'H');
        let res = parser.parse(input);
        assert!(res.is_ok());
        assert_eq!(res.unwrap().0, 'H');
    }
}
