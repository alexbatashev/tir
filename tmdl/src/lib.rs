mod ast;
mod compiler;
mod error;
mod lexer;
mod parser;
mod rocqgen;
mod rustgen;
mod sailproofgen;
mod sema;

use chumsky::prelude::*;

pub type Span = SimpleSpan;
pub type Spanned<T> = (T, Span);

pub use compiler::{Action, Compiler, OutputKind, compiler_main};

pub use lexer::lex;
pub use parser::parse;
pub use sema::analyze as sema_analyze;
