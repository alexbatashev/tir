use std::fmt;

/// A raw bit vector that can hold arbitrary numbers of bits (including 2048+ bits)
/// This is used for reinterpret casts between different types
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitVec {
    /// The number of bits in this vector
    width: usize,
    /// The raw bits, stored as a vector of u64 chunks
    /// Length is (width + 63) / 64
    data: Vec<u64>,
}

impl BitVec {
    /// Create a new BitVec with the given width, initialized to zero
    pub fn new(width: usize) -> Self {
        assert!(width > 0, "Width must be at least 1 bit");

        let num_words = (width + 63) / 64;
        let data = vec![0u64; num_words];

        BitVec { width, data }
    }

    /// Create a new BitVec from a u128 value
    pub fn from_u128(width: usize, value: u128) -> Self {
        assert!(width > 0, "Width must be at least 1 bit");
        assert!(width <= 128, "Use from_bytes for widths > 128");

        let mut bv = Self::new(width);
        if bv.data.len() > 0 {
            bv.data[0] = value as u64;
        }
        if bv.data.len() > 1 {
            bv.data[1] = (value >> 64) as u64;
        }

        bv.mask_unused_bits();
        bv
    }

    /// Create a BitVec from a byte slice (little-endian)
    pub fn from_bytes(width: usize, bytes: &[u8]) -> Self {
        assert!(width > 0, "Width must be at least 1 bit");

        let mut bv = Self::new(width);
        let num_bytes = (width + 7) / 8;
        let available_bytes = bytes.len().min(num_bytes);

        for i in 0..available_bytes {
            let word_idx = i / 8;
            let byte_in_word = i % 8;
            bv.data[word_idx] |= (bytes[i] as u64) << (byte_in_word * 8);
        }

        bv.mask_unused_bits();
        bv
    }

    /// Get the bit width
    pub fn width(&self) -> usize {
        self.width
    }

    /// Convert to u128 (for BitVecs up to 128 bits)
    pub fn to_u128(&self) -> u128 {
        assert!(self.width <= 128, "BitVec too large to convert to u128");

        let low = if self.data.len() > 0 { self.data[0] } else { 0 };
        let high = if self.data.len() > 1 { self.data[1] } else { 0 };

        ((high as u128) << 64) | (low as u128)
    }

    /// Convert to a byte vector (little-endian)
    pub fn to_bytes(&self) -> Vec<u8> {
        let num_bytes = (self.width + 7) / 8;
        let mut bytes = Vec::with_capacity(num_bytes);

        for i in 0..num_bytes {
            let word_idx = i / 8;
            let byte_in_word = i % 8;
            let byte = if word_idx < self.data.len() {
                ((self.data[word_idx] >> (byte_in_word * 8)) & 0xFF) as u8
            } else {
                0
            };
            bytes.push(byte);
        }

        bytes
    }

    /// Get a specific bit (0-indexed from LSB)
    pub fn get_bit(&self, index: usize) -> bool {
        assert!(index < self.width, "Bit index out of range");

        let word_idx = index / 64;
        let bit_in_word = index % 64;

        (self.data[word_idx] >> bit_in_word) & 1 == 1
    }

    /// Set a specific bit (0-indexed from LSB)
    pub fn set_bit(&mut self, index: usize, value: bool) {
        assert!(index < self.width, "Bit index out of range");

        let word_idx = index / 64;
        let bit_in_word = index % 64;

        if value {
            self.data[word_idx] |= 1u64 << bit_in_word;
        } else {
            self.data[word_idx] &= !(1u64 << bit_in_word);
        }
    }

    /// Extract a range of bits [high:low] (inclusive) and return as a new BitVec
    pub fn extract(&self, high_bit: usize, low_bit: usize) -> Self {
        assert!(high_bit >= low_bit, "High bit must be >= low bit");
        assert!(high_bit < self.width, "High bit out of range");

        let new_width = high_bit - low_bit + 1;
        let mut result = Self::new(new_width);

        for i in 0..new_width {
            let src_bit = low_bit + i;
            if self.get_bit(src_bit) {
                result.set_bit(i, true);
            }
        }

        result
    }

    /// Concatenate two BitVecs (self becomes high bits, other becomes low bits)
    pub fn concat(&self, other: &BitVec) -> Self {
        let new_width = self.width + other.width;
        let mut result = Self::new(new_width);

        // Copy other's bits to lower positions
        for i in 0..other.width {
            if other.get_bit(i) {
                result.set_bit(i, true);
            }
        }

        // Copy self's bits to higher positions
        for i in 0..self.width {
            if self.get_bit(i) {
                result.set_bit(other.width + i, true);
            }
        }

        result
    }

    /// Resize the BitVec to a new width (zero-extend or truncate)
    pub fn resize(&self, new_width: usize) -> Self {
        assert!(new_width > 0, "Width must be at least 1 bit");

        if new_width == self.width {
            return self.clone();
        }

        let mut result = Self::new(new_width);
        let copy_width = new_width.min(self.width);

        for i in 0..copy_width {
            if self.get_bit(i) {
                result.set_bit(i, true);
            }
        }

        result
    }

    /// Helper function to mask unused bits in the highest word
    fn mask_unused_bits(&mut self) {
        if self.data.is_empty() {
            return;
        }

        let bits_in_last_word = self.width % 64;
        if bits_in_last_word != 0 {
            let last_idx = self.data.len() - 1;
            let mask = (1u64 << bits_in_last_word) - 1;
            self.data[last_idx] &= mask;
        }
    }
}

impl fmt::Display for BitVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0b")?;
        for i in (0..self.width).rev() {
            let bit = if self.get_bit(i) { '1' } else { '0' };
            write!(f, "{}", bit)?;
        }
        Ok(())
    }
}

impl fmt::Binary for BitVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in (0..self.width).rev() {
            let bit = if self.get_bit(i) { '1' } else { '0' };
            write!(f, "{}", bit)?;
        }
        Ok(())
    }
}

impl fmt::LowerHex for BitVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Print hex nibbles from high to low
        let num_nibbles = (self.width + 3) / 4;
        for nibble_idx in (0..num_nibbles).rev() {
            let bit_offset = nibble_idx * 4;
            let mut nibble = 0u8;
            for i in 0..4 {
                let bit_pos = bit_offset + i;
                if bit_pos < self.width && self.get_bit(bit_pos) {
                    nibble |= 1 << i;
                }
            }
            write!(f, "{:x}", nibble)?;
        }
        Ok(())
    }
}

impl fmt::UpperHex for BitVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Print hex nibbles from high to low
        let num_nibbles = (self.width + 3) / 4;
        for nibble_idx in (0..num_nibbles).rev() {
            let bit_offset = nibble_idx * 4;
            let mut nibble = 0u8;
            for i in 0..4 {
                let bit_pos = bit_offset + i;
                if bit_pos < self.width && self.get_bit(bit_pos) {
                    nibble |= 1 << i;
                }
            }
            write!(f, "{:X}", nibble)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let bv = BitVec::new(8);
        assert_eq!(bv.width(), 8);
        assert_eq!(bv.to_u128(), 0);
    }

    #[test]
    fn test_from_u128() {
        let bv = BitVec::from_u128(8, 0b11010110);
        assert_eq!(bv.width(), 8);
        assert_eq!(bv.to_u128(), 0b11010110);
    }

    #[test]
    fn test_from_u128_large() {
        let bv = BitVec::from_u128(128, 0x123456789ABCDEF0_FEDCBA9876543210u128);
        assert_eq!(bv.width(), 128);
        assert_eq!(bv.to_u128(), 0x123456789ABCDEF0_FEDCBA9876543210u128);
    }

    #[test]
    fn test_masking() {
        // Create with more bits than width - should be masked
        let bv = BitVec::from_u128(4, 0xFF);
        assert_eq!(bv.to_u128(), 0xF); // Only lower 4 bits
    }

    #[test]
    fn test_get_bit() {
        let bv = BitVec::from_u128(8, 0b11010110);
        assert!(!bv.get_bit(0));
        assert!(bv.get_bit(1));
        assert!(bv.get_bit(2));
        assert!(!bv.get_bit(3));
        assert!(bv.get_bit(4));
        assert!(!bv.get_bit(5));
        assert!(bv.get_bit(6));
        assert!(bv.get_bit(7));
    }

    #[test]
    fn test_set_bit() {
        let mut bv = BitVec::new(8);
        bv.set_bit(0, true);
        bv.set_bit(2, true);
        bv.set_bit(4, true);
        assert_eq!(bv.to_u128(), 0b00010101);

        bv.set_bit(2, false);
        assert_eq!(bv.to_u128(), 0b00010001);
    }

    #[test]
    fn test_extract() {
        let bv = BitVec::from_u128(8, 0b11010110);
        let extracted = bv.extract(5, 2);
        assert_eq!(extracted.width(), 4);
        assert_eq!(extracted.to_u128(), 0b0101);
    }

    #[test]
    fn test_concat() {
        let bv1 = BitVec::from_u128(4, 0b1010);
        let bv2 = BitVec::from_u128(4, 0b0101);
        let result = bv1.concat(&bv2);
        assert_eq!(result.width(), 8);
        assert_eq!(result.to_u128(), 0b10100101);
    }

    #[test]
    fn test_resize_extend() {
        let bv = BitVec::from_u128(8, 0xFF);
        let resized = bv.resize(16);
        assert_eq!(resized.width(), 16);
        assert_eq!(resized.to_u128(), 0xFF); // Zero-extended
    }

    #[test]
    fn test_resize_truncate() {
        let bv = BitVec::from_u128(16, 0xABCD);
        let resized = bv.resize(8);
        assert_eq!(resized.width(), 8);
        assert_eq!(resized.to_u128(), 0xCD); // Truncated to lower 8 bits
    }

    #[test]
    fn test_display() {
        let bv = BitVec::from_u128(8, 0b11010110);
        assert_eq!(format!("{}", bv), "0b11010110");
    }

    #[test]
    fn test_large_bitvec_2048() {
        // Test 2048-bit BitVec
        let bv = BitVec::new(2048);
        assert_eq!(bv.width(), 2048);
        assert_eq!(bv.data.len(), 32); // 2048 / 64 = 32 words
    }

    #[test]
    fn test_large_bitvec_non_power_of_2() {
        // Test non-power-of-2 width (2049 bits)
        let mut bv = BitVec::new(2049);
        assert_eq!(bv.width(), 2049);
        assert_eq!(bv.data.len(), 33); // ceiling(2049 / 64) = 33 words

        // Set some bits and verify
        bv.set_bit(0, true);
        bv.set_bit(2048, true);
        assert!(bv.get_bit(0));
        assert!(bv.get_bit(2048));
        assert!(!bv.get_bit(1));
    }

    #[test]
    fn test_from_bytes() {
        let bytes = vec![0xCD, 0xAB, 0x12, 0x34];
        let bv = BitVec::from_bytes(32, &bytes);
        assert_eq!(bv.width(), 32);
        assert_eq!(bv.to_u128(), 0x3412ABCD); // Little-endian
    }

    #[test]
    fn test_to_bytes() {
        let bv = BitVec::from_u128(32, 0x3412ABCD);
        let bytes = bv.to_bytes();
        assert_eq!(bytes, vec![0xCD, 0xAB, 0x12, 0x34]); // Little-endian
    }
}
