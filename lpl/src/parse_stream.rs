use std::ops::Range;

pub trait ParseStream<'a>: Clone {
    type Slice;

    fn get(&self, range: Range<usize>) -> Option<Self::Slice>;
    fn slice(&self, range: Range<usize>) -> Option<Self>
    where
        Self: Sized;
    fn len(&self) -> usize;

    fn is_string_like(&self) -> bool {
        false
    }

    fn chars(&self) -> std::str::Chars<'_> {
        unimplemented!()
    }

    fn substr(&self, _range: Range<usize>) -> Option<&'a str> {
        unimplemented!()
    }
}

#[derive(Debug, Clone)]
pub struct StrStream<'a> {
    string: &'a str,
}

impl<'a> ParseStream<'a> for StrStream<'a> {
    type Slice = &'a str;

    fn get(&self, range: Range<usize>) -> Option<Self::Slice> {
        self.string.get(range)
    }

    fn slice(&self, range: Range<usize>) -> Option<Self> {
        self.string.get(range).map(|string| Self { string })
    }

    fn len(&self) -> usize {
        self.string.len()
    }

    fn is_string_like(&self) -> bool {
        true
    }

    fn chars(&self) -> std::str::Chars<'_> {
        self.string.chars()
    }

    fn substr(&self, range: Range<usize>) -> Option<&'a str> {
        self.string.get(range)
    }
}

impl<'a> From<&'a str> for StrStream<'a> {
    fn from(value: &'a str) -> Self {
        StrStream { string: value }
    }
}
