use lpl::combinators::literal;
use lpl::{ParseStream, Parser};

use crate::{Ident, Item, ItemEnum, Token, TokenStream};

pub fn parse<'a>(tokens: &'a [Token<'a>]) {
    let token_stream = TokenStream::new(tokens);
}

// fn item<'a>() -> impl Parser<'a, TokenStream<'a>, Item> {
//
// }

fn enum_<'a>() -> impl Parser<'a, TokenStream<'a>, ItemEnum> {
    literal_token(Token::Enum)
        .and_then(ident())
        .map(|(_, ident)| ident)
        .and_then(literal_token(Token::BraceOpen))
        .map(|(ident, _)| ident)
        .map(|_| ItemEnum {
            name: Ident("some".to_string()),
            variants: vec![],
        })
}

fn literal_token<'a>(token: Token<'static>) -> impl Parser<'a, TokenStream<'a>, ()> {
    move |stream: TokenStream<'a>| {
        if stream.get(0..1).unwrap()[0] == token {
            Ok(((), stream.slice(1..stream.len())))
        } else {
            Err("".to_string())
        }
    }
}

fn ident<'a>() -> impl Parser<'a, TokenStream<'a>, Ident> {
    move |stream: TokenStream<'a>| match stream.get(0..1).unwrap()[0] {
        Token::Ident(ident) => Ok((Ident(ident.to_string()), stream.slice(1..stream.len()))),
        _ => Err("expected ident".to_string()),
    }
}
