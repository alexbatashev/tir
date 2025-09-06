#[derive(Debug, Copy, Clone)]
pub struct Span(pub u32);

pub trait Cursor {
    fn span(&self) -> Span;
    fn skip_trivia(&mut self);
}

