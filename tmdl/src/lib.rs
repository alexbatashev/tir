mod ast;
mod lexer;

use chumsky::prelude::*;

pub type Span = SimpleSpan;
pub type Spanned<T> = (T, Span);
