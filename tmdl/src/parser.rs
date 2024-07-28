use lpl::combinators::{one_or_more, separated};
use lpl::{ParseStream, Parser};

use crate::{Ident, Item, ItemEnum, ItemRecord, Token, TokenStream, TranslationUnit};

pub fn parse<'a>(tokens: &'a [Token<'a>]) -> TranslationUnit {
    let token_stream = TokenStream::new(tokens);

    let items_parser = one_or_more(item());
    let items = items_parser.parse(token_stream).unwrap();

    TranslationUnit { items: items.0 }
}

fn item<'a>() -> impl Parser<'a, TokenStream<'a>, Item> {
    enum_()
        .map(|enum_| Item::Enum(enum_))
        .or_else(record().map(|record| Item::Record(record)))
}

fn record<'a>() -> impl Parser<'a, TokenStream<'a>, ItemRecord> {
    literal_token(Token::Record).map(|_| ItemRecord {})
}

fn enum_<'a>() -> impl Parser<'a, TokenStream<'a>, ItemEnum> {
    literal_token(Token::Enum)
        .and_then(ident())
        .map(|(_, ident)| ident)
        .and_then(literal_token(Token::BraceOpen))
        .map(|(ident, _)| ident)
        .and_then(separated(ident(), literal_token(Token::Comma)))
        .and_then(literal_token(Token::BraceClose))
        .map(|(result, _)| result)
        .map(|(name, variants)| ItemEnum { name, variants })
}

fn literal_token<'a>(token: Token<'static>) -> impl Parser<'a, TokenStream<'a>, ()> {
    move |stream: TokenStream<'a>| {
        if stream.len() == 0 {
            return Err("Empty stream".to_string());
        }

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
        _ => Err("expected an identifier".to_string()),
    }
}
