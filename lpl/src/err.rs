use crate::Span;

#[derive(Debug)]
pub struct ParserError {
    message: String,
    span: Span,
}

impl ParserError {
    pub fn new(message: String, span: Span) -> Self {
        Self { message, span }
    }

    pub fn take_message(self) -> String {
        self.message
    }
}
