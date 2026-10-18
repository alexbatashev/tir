use std::hash::{BuildHasherDefault, Hasher};

#[cfg(target_pointer_width = "64")]
const K: usize = 0x517c_c1b7_2722_0a95;
#[cfg(target_pointer_width = "32")]
const K: usize = 0x9e37_79b9;

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;

#[derive(Default)]
pub struct FxHasher {
    hash: usize,
}

impl FxHasher {
    #[inline]
    fn add_to_hash(&mut self, value: usize) {
        self.hash = (self.hash.rotate_left(5) ^ value).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash as u64
    }

    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        const WORD_BYTES: usize = size_of::<usize>();

        while bytes.len() >= WORD_BYTES {
            let (word, rest) = bytes.split_at(WORD_BYTES);
            self.add_to_hash(usize::from_ne_bytes(word.try_into().unwrap()));
            bytes = rest;
        }

        if bytes.len() >= 4 {
            let (word, rest) = bytes.split_at(4);
            self.add_to_hash(u32::from_ne_bytes(word.try_into().unwrap()) as usize);
            bytes = rest;
        }
        if bytes.len() >= 2 {
            let (word, rest) = bytes.split_at(2);
            self.add_to_hash(u16::from_ne_bytes(word.try_into().unwrap()) as usize);
            bytes = rest;
        }
        if let Some(&byte) = bytes.first() {
            self.add_to_hash(byte as usize);
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add_to_hash(value as usize);
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add_to_hash(value as usize);
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add_to_hash(value as usize);
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        #[cfg(target_pointer_width = "64")]
        self.add_to_hash(value as usize);
        #[cfg(target_pointer_width = "32")]
        {
            self.add_to_hash(value as usize);
            self.add_to_hash((value >> 32) as usize);
        }
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add_to_hash(value);
    }
}

#[cfg(test)]
mod tests {
    use std::hash::Hasher;

    use super::{FxHasher, K};

    #[test]
    fn hashes_words_with_fx_recurrence() {
        let mut hasher = FxHasher::default();
        hasher.write_usize(1);
        hasher.write_usize(2);

        let first = K;
        let expected = (first.rotate_left(5) ^ 2).wrapping_mul(K);
        assert_eq!(hasher.finish(), expected as u64);
    }

    #[test]
    fn hashes_byte_slices_in_native_endian_words() {
        let bytes: Vec<_> = (0..size_of::<usize>()).map(|byte| byte as u8).collect();
        let word = usize::from_ne_bytes(bytes.clone().try_into().unwrap());
        let mut hasher = FxHasher::default();

        hasher.write(&bytes);

        assert_eq!(hasher.finish(), word.wrapping_mul(K) as u64);
    }
}
