mod ast;
mod compiler;
mod lexer;
mod parser;

use chumsky::prelude::*;

pub type Span = SimpleSpan;
pub type Spanned<T> = (T, Span);

pub use compiler::{Action, Compiler, OutputKind, compiler_main};

pub use lexer::lex;
pub use parser::parse;
