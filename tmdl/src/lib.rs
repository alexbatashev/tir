mod ast;
mod lexer;
mod parser;

use chumsky::prelude::*;

pub type Span = SimpleSpan;
pub type Spanned<T> = (T, Span);
