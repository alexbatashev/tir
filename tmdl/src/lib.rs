mod ast;
mod compiler;
mod lexer;
mod parser;

use chumsky::prelude::*;

pub type Span = SimpleSpan;
pub type Spanned<T> = (T, Span);

pub use compiler::compiler_main;
