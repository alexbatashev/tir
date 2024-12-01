use lpl::Diagnostic;

use crate::ast::SourceFile;

pub fn check_tmdl_sema(ast: Vec<SourceFile>) -> (Vec<SourceFile>, Vec<Diagnostic>) {
    // TODO perform proper semantic analysis
    (ast, vec![])
}
