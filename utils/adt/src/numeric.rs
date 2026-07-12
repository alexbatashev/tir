pub trait AsIndex: Copy {
    fn as_index(self) -> usize;
    fn from_usize(v: usize) -> Self;
}

impl AsIndex for usize {
    fn as_index(self) -> usize {
        self
    }

    fn from_usize(v: usize) -> Self {
        v
    }
}

macro_rules! define_idx {
    ($t:ty) => {
        impl AsIndex for $t {
            fn as_index(self) -> usize {
                self as usize
            }
            fn from_usize(v: usize) -> Self {
                v as Self
            }
        }
    };
}

define_idx!(u64);
define_idx!(u32);
define_idx!(u16);
define_idx!(u8);
