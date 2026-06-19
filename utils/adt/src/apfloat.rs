use std::cmp::Ordering;
use std::fmt;

/// Arbitrary Precision Floating Point
/// Supports any combination of exponent and mantissa widths
/// Can represent IEEE 754, BF16, FP8 variants, x86 extended, and custom formats
#[derive(Clone, Debug)]
pub struct APFloat {
    /// Number of exponent bits
    exp_width: u32,
    /// Number of mantissa bits (excluding implicit leading bit, unless explicit)
    mant_width: u32,
    /// Whether the mantissa has an explicit leading bit (like x86 80-bit)
    explicit_leading_bit: bool,
    /// Sign bit (false = positive, true = negative)
    sign: bool,
    /// Biased exponent value
    exponent: u32,
    /// Mantissa bits (stored in lower bits, may need 128 bits for large formats)
    mantissa_high: u64, // Upper 64 bits of mantissa
    mantissa_low: u64, // Lower 64 bits of mantissa
}

impl APFloat {
    /// Create a new APFloat with custom exponent and mantissa widths
    pub fn new(exp_width: u32, mant_width: u32, explicit_leading_bit: bool) -> Self {
        assert!(
            exp_width > 0 && exp_width <= 32,
            "Exponent width must be 1-32 bits"
        );
        assert!(
            mant_width > 0 && mant_width <= 128,
            "Mantissa width must be 1-128 bits"
        );

        APFloat {
            exp_width,
            mant_width,
            explicit_leading_bit,
            sign: false,
            exponent: 0,
            mantissa_high: 0,
            mantissa_low: 0,
        }
    }

    /// Create from raw bit representation
    pub fn from_bits(
        exp_width: u32,
        mant_width: u32,
        explicit_leading_bit: bool,
        bits: u128,
    ) -> Self {
        assert!(
            exp_width > 0 && exp_width <= 32,
            "Exponent width must be 1-32 bits"
        );
        assert!(
            mant_width > 0 && mant_width <= 128,
            "Mantissa width must be 1-128 bits"
        );

        let total_width = 1 + exp_width + mant_width;
        assert!(total_width <= 128, "Total width exceeds 128 bits");

        let sign_bit = total_width - 1;
        let sign = (bits >> sign_bit) & 1 == 1;

        let exp_mask = (1u32 << exp_width) - 1;
        let exponent = ((bits >> mant_width) as u32) & exp_mask;

        let mantissa_low = bits as u64;
        let mantissa_high = if mant_width > 64 {
            (bits >> 64) as u64
        } else {
            0
        };

        // Mask to only the mantissa bits
        let low_mask = if mant_width >= 64 {
            u64::MAX
        } else {
            (1u64 << mant_width) - 1
        };

        let high_mask = if mant_width > 64 {
            (1u64 << (mant_width - 64)) - 1
        } else {
            0
        };

        APFloat {
            exp_width,
            mant_width,
            explicit_leading_bit,
            sign,
            exponent,
            mantissa_high: mantissa_high & high_mask,
            mantissa_low: mantissa_low & low_mask,
        }
    }

    // ============ Common Format Constructors ============

    /// IEEE 754 binary16 (half precision): 1 sign, 5 exp, 10 mantissa
    pub fn half() -> Self {
        Self::new(5, 10, false)
    }

    /// BFloat16 (Brain Float): 1 sign, 8 exp, 7 mantissa
    pub fn bfloat16() -> Self {
        Self::new(8, 7, false)
    }

    /// IEEE 754 binary32 (single precision): 1 sign, 8 exp, 23 mantissa
    pub fn single() -> Self {
        Self::new(8, 23, false)
    }

    /// IEEE 754 binary64 (double precision): 1 sign, 11 exp, 52 mantissa
    pub fn double() -> Self {
        Self::new(11, 52, false)
    }

    /// x86 80-bit extended precision: 1 sign, 15 exp, 64 mantissa (explicit)
    pub fn x86_extended() -> Self {
        Self::new(15, 64, true)
    }

    /// IEEE 754 binary128 (quad precision): 1 sign, 15 exp, 112 mantissa
    pub fn quad() -> Self {
        Self::new(15, 112, false)
    }

    /// FP8 E4M3 (4-bit exponent, 3-bit mantissa)
    pub fn fp8_e4m3() -> Self {
        Self::new(4, 3, false)
    }

    /// FP8 E5M2 (5-bit exponent, 2-bit mantissa)
    pub fn fp8_e5m2() -> Self {
        Self::new(5, 2, false)
    }

    // ============ Getters ============

    /// Get the exponent width
    pub fn exp_width(&self) -> u32 {
        self.exp_width
    }

    /// Get the mantissa width
    pub fn mant_width(&self) -> u32 {
        self.mant_width
    }

    /// Get the total bit width
    pub fn bit_width(&self) -> u32 {
        1 + self.exp_width + self.mant_width
    }

    /// Check if this has an explicit leading bit
    pub fn has_explicit_leading_bit(&self) -> bool {
        self.explicit_leading_bit
    }

    /// Get the exponent bias (standard: 2^(exp_width-1) - 1)
    pub fn exponent_bias(&self) -> i32 {
        (1i32 << (self.exp_width - 1)) - 1
    }

    // ============ Value Construction ============

    /// Create a zero value
    pub fn zero(
        exp_width: u32,
        mant_width: u32,
        explicit_leading_bit: bool,
        negative: bool,
    ) -> Self {
        APFloat {
            exp_width,
            mant_width,
            explicit_leading_bit,
            sign: negative,
            exponent: 0,
            mantissa_high: 0,
            mantissa_low: 0,
        }
    }

    /// Create positive or negative infinity
    pub fn infinity(
        exp_width: u32,
        mant_width: u32,
        explicit_leading_bit: bool,
        negative: bool,
    ) -> Self {
        let exp_max = (1u32 << exp_width) - 1;
        APFloat {
            exp_width,
            mant_width,
            explicit_leading_bit,
            sign: negative,
            exponent: exp_max,
            mantissa_high: 0,
            mantissa_low: 0,
        }
    }

    /// Create NaN (quiet NaN with highest mantissa bit set)
    pub fn nan(exp_width: u32, mant_width: u32, explicit_leading_bit: bool) -> Self {
        let exp_max = (1u32 << exp_width) - 1;
        // Set the highest mantissa bit for quiet NaN
        let (mant_high, mant_low) = if mant_width > 64 {
            (1u64 << (mant_width - 64 - 1), 0)
        } else {
            (0, 1u64 << (mant_width - 1))
        };

        APFloat {
            exp_width,
            mant_width,
            explicit_leading_bit,
            sign: false,
            exponent: exp_max,
            mantissa_high: mant_high,
            mantissa_low: mant_low,
        }
    }

    // ============ Conversions ============

    /// Convert to raw bit representation
    pub fn to_bits(&self) -> u128 {
        let sign_bit = if self.sign { 1u128 } else { 0u128 };
        let sign_shifted = sign_bit << (self.bit_width() - 1);

        let exp_shifted = (self.exponent as u128) << self.mant_width;

        let mantissa = if self.mant_width > 64 {
            ((self.mantissa_high as u128) << 64) | (self.mantissa_low as u128)
        } else {
            self.mantissa_low as u128
        };

        sign_shifted | exp_shifted | mantissa
    }

    /// Create from f32 (creates a single precision APFloat)
    pub fn from_f32(value: f32) -> Self {
        Self::from_bits(8, 23, false, value.to_bits() as u128)
    }

    /// Create from f64 (creates a double precision APFloat)
    pub fn from_f64(value: f64) -> Self {
        Self::from_bits(11, 52, false, value.to_bits() as u128)
    }

    /// Convert to f32 (may lose precision or be inaccurate for non-standard formats)
    pub fn to_f32(&self) -> f32 {
        // For single precision, direct conversion
        if self.exp_width == 8 && self.mant_width == 23 && !self.explicit_leading_bit {
            return f32::from_bits(self.to_bits() as u32);
        }

        // For other formats, convert to double first if possible, then to single
        self.to_f64() as f32
    }

    /// Convert to f64 (may lose precision for quad/extended formats)
    pub fn to_f64(&self) -> f64 {
        // For double precision, direct conversion
        if self.exp_width == 11 && self.mant_width == 52 && !self.explicit_leading_bit {
            return f64::from_bits(self.to_bits() as u64);
        }

        // Handle special cases
        if self.is_nan() {
            return f64::NAN;
        }
        if self.is_infinity() {
            return if self.sign {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
        }
        if self.is_zero() {
            return if self.sign { -0.0 } else { 0.0 };
        }

        // Convert to double precision format
        // This is approximate and may lose precision
        let converted = self.convert(11, 52, false);
        f64::from_bits(converted.to_bits() as u64)
    }

    /// Convert to a different floating-point format
    pub fn convert(&self, new_exp_width: u32, new_mant_width: u32, new_explicit: bool) -> Self {
        // If same format, return clone
        if self.exp_width == new_exp_width
            && self.mant_width == new_mant_width
            && self.explicit_leading_bit == new_explicit
        {
            return self.clone();
        }

        // Handle special values
        if self.is_nan() {
            return Self::nan(new_exp_width, new_mant_width, new_explicit);
        }
        if self.is_infinity() {
            return Self::infinity(new_exp_width, new_mant_width, new_explicit, self.sign);
        }
        if self.is_zero() {
            return Self::zero(new_exp_width, new_mant_width, new_explicit, self.sign);
        }

        // Convert the exponent
        let source_bias = self.exponent_bias();
        let target_bias = (1i32 << (new_exp_width - 1)) - 1;

        let unbiased_exp = (self.exponent as i32) - source_bias;
        let new_biased_exp = unbiased_exp + target_bias;

        // Check for overflow/underflow
        let target_exp_max = (1i32 << new_exp_width) - 1;
        let new_exponent = if new_biased_exp <= 0 {
            // Underflow - becomes zero or denormal
            0
        } else if new_biased_exp >= target_exp_max {
            // Overflow - becomes infinity
            return Self::infinity(new_exp_width, new_mant_width, new_explicit, self.sign);
        } else {
            new_biased_exp as u32
        };

        // Convert the mantissa
        let (new_mant_high, new_mant_low) = if new_mant_width > self.mant_width {
            // Extending mantissa - shift left and zero-fill
            let shift = new_mant_width - self.mant_width;
            self.shift_mantissa_left(shift)
        } else if new_mant_width < self.mant_width {
            // Truncating mantissa - shift right (with rounding if desired)
            let shift = self.mant_width - new_mant_width;
            self.shift_mantissa_right(shift)
        } else {
            (self.mantissa_high, self.mantissa_low)
        };

        APFloat {
            exp_width: new_exp_width,
            mant_width: new_mant_width,
            explicit_leading_bit: new_explicit,
            sign: self.sign,
            exponent: new_exponent,
            mantissa_high: new_mant_high,
            mantissa_low: new_mant_low,
        }
    }

    // ============ Predicates ============

    /// Check if this is zero
    pub fn is_zero(&self) -> bool {
        self.exponent == 0 && self.mantissa_high == 0 && self.mantissa_low == 0
    }

    /// Check if this is infinity
    pub fn is_infinity(&self) -> bool {
        let exp_max = (1u32 << self.exp_width) - 1;
        self.exponent == exp_max && self.mantissa_high == 0 && self.mantissa_low == 0
    }

    /// Check if this is NaN
    pub fn is_nan(&self) -> bool {
        let exp_max = (1u32 << self.exp_width) - 1;
        self.exponent == exp_max && (self.mantissa_high != 0 || self.mantissa_low != 0)
    }

    /// Check if this is negative
    pub fn is_negative(&self) -> bool {
        self.sign
    }

    /// Check if this is a denormal number
    pub fn is_denormal(&self) -> bool {
        self.exponent == 0 && (self.mantissa_high != 0 || self.mantissa_low != 0)
    }

    // ============ Arithmetic Operations ============

    /// Negate the value
    pub fn neg(&self) -> Self {
        APFloat {
            exp_width: self.exp_width,
            mant_width: self.mant_width,
            explicit_leading_bit: self.explicit_leading_bit,
            sign: !self.sign,
            exponent: self.exponent,
            mantissa_high: self.mantissa_high,
            mantissa_low: self.mantissa_low,
        }
    }

    /// Absolute value
    pub fn abs(&self) -> Self {
        APFloat {
            exp_width: self.exp_width,
            mant_width: self.mant_width,
            explicit_leading_bit: self.explicit_leading_bit,
            sign: false,
            exponent: self.exponent,
            mantissa_high: self.mantissa_high,
            mantissa_low: self.mantissa_low,
        }
    }

    /// Add two floating-point numbers
    /// Note: This uses native f64 arithmetic, which may lose precision for some formats
    pub fn add(&self, other: &APFloat) -> Self {
        assert_eq!(
            self.exp_width, other.exp_width,
            "Exponent widths must match"
        );
        assert_eq!(
            self.mant_width, other.mant_width,
            "Mantissa widths must match"
        );

        // Use native arithmetic through f64
        let result = self.to_f64() + other.to_f64();
        let result_float = Self::from_f64(result);
        result_float.convert(self.exp_width, self.mant_width, self.explicit_leading_bit)
    }

    /// Subtract two floating-point numbers
    pub fn sub(&self, other: &APFloat) -> Self {
        self.add(&other.neg())
    }

    /// Multiply two floating-point numbers
    pub fn mul(&self, other: &APFloat) -> Self {
        assert_eq!(
            self.exp_width, other.exp_width,
            "Exponent widths must match"
        );
        assert_eq!(
            self.mant_width, other.mant_width,
            "Mantissa widths must match"
        );

        let result = self.to_f64() * other.to_f64();
        let result_float = Self::from_f64(result);
        result_float.convert(self.exp_width, self.mant_width, self.explicit_leading_bit)
    }

    /// Divide two floating-point numbers
    pub fn div(&self, other: &APFloat) -> Self {
        assert_eq!(
            self.exp_width, other.exp_width,
            "Exponent widths must match"
        );
        assert_eq!(
            self.mant_width, other.mant_width,
            "Mantissa widths must match"
        );

        let result = self.to_f64() / other.to_f64();
        let result_float = Self::from_f64(result);
        result_float.convert(self.exp_width, self.mant_width, self.explicit_leading_bit)
    }

    /// Square root
    pub fn sqrt(&self) -> Self {
        let result = self.to_f64().sqrt();
        let result_float = Self::from_f64(result);
        result_float.convert(self.exp_width, self.mant_width, self.explicit_leading_bit)
    }

    /// Fused multiply-add: (self * b) + c
    pub fn fma(&self, b: &APFloat, c: &APFloat) -> Self {
        assert_eq!(self.exp_width, b.exp_width, "Exponent widths must match");
        assert_eq!(self.mant_width, b.mant_width, "Mantissa widths must match");
        assert_eq!(self.exp_width, c.exp_width, "Exponent widths must match");
        assert_eq!(self.mant_width, c.mant_width, "Mantissa widths must match");

        let result = self.to_f64().mul_add(b.to_f64(), c.to_f64());
        let result_float = Self::from_f64(result);
        result_float.convert(self.exp_width, self.mant_width, self.explicit_leading_bit)
    }

    // ============ Comparison ============

    /// Compare two floating-point numbers
    pub fn compare(&self, other: &APFloat) -> Option<Ordering> {
        // NaN comparisons are unordered
        if self.is_nan() || other.is_nan() {
            return None;
        }

        // Handle zeros
        if self.is_zero() && other.is_zero() {
            return Some(Ordering::Equal);
        }

        // Handle infinities
        if self.is_infinity() && other.is_infinity() {
            if self.sign == other.sign {
                return Some(Ordering::Equal);
            } else {
                return Some(if self.sign {
                    Ordering::Less
                } else {
                    Ordering::Greater
                });
            }
        }

        // Use f64 comparison (may lose precision for some formats)
        self.to_f64().partial_cmp(&other.to_f64())
    }

    /// Less than
    pub fn lt(&self, other: &APFloat) -> bool {
        matches!(self.compare(other), Some(Ordering::Less))
    }

    /// Less than or equal
    pub fn le(&self, other: &APFloat) -> bool {
        matches!(self.compare(other), Some(Ordering::Less | Ordering::Equal))
    }

    /// Greater than
    pub fn gt(&self, other: &APFloat) -> bool {
        matches!(self.compare(other), Some(Ordering::Greater))
    }

    /// Greater than or equal
    pub fn ge(&self, other: &APFloat) -> bool {
        matches!(
            self.compare(other),
            Some(Ordering::Greater | Ordering::Equal)
        )
    }

    // ============ Helper Functions ============

    fn shift_mantissa_left(&self, shift: u32) -> (u64, u64) {
        if shift == 0 {
            return (self.mantissa_high, self.mantissa_low);
        }

        if shift >= 128 {
            return (0, 0);
        }

        if shift < 64 {
            let new_low = self.mantissa_low << shift;
            let new_high = (self.mantissa_high << shift) | (self.mantissa_low >> (64 - shift));
            (new_high, new_low)
        } else {
            let new_high = self.mantissa_low << (shift - 64);
            (new_high, 0)
        }
    }

    fn shift_mantissa_right(&self, shift: u32) -> (u64, u64) {
        if shift == 0 {
            return (self.mantissa_high, self.mantissa_low);
        }

        if shift >= 128 {
            return (0, 0);
        }

        if shift < 64 {
            let new_low = (self.mantissa_low >> shift) | (self.mantissa_high << (64 - shift));
            let new_high = self.mantissa_high >> shift;
            (new_high, new_low)
        } else {
            let new_low = self.mantissa_high >> (shift - 64);
            (0, new_low)
        }
    }
}

impl PartialEq for APFloat {
    fn eq(&self, other: &Self) -> bool {
        matches!(self.compare(other), Some(Ordering::Equal))
    }
}

impl fmt::Display for APFloat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_nan() {
            write!(f, "NaN")
        } else if self.is_infinity() {
            write!(f, "{}inf", if self.sign { "-" } else { "" })
        } else {
            write!(f, "{}", self.to_f64())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Equality that treats any two NaNs as equal (f64 NaN != NaN otherwise).
    fn f64_eq(a: f64, b: f64) -> bool {
        a == b || (a.is_nan() && b.is_nan())
    }

    proptest! {
        #[test]
        fn test_add(x in prop::num::f64::ANY, y in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).add(&APFloat::from_f64(y));
            prop_assert!(f64_eq(res.to_f64(), x + y));
        }

        #[test]
        fn test_sub(x in prop::num::f64::ANY, y in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).sub(&APFloat::from_f64(y));
            prop_assert!(f64_eq(res.to_f64(), x - y));
        }

        #[test]
        fn test_mul(x in prop::num::f64::ANY, y in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).mul(&APFloat::from_f64(y));
            prop_assert!(f64_eq(res.to_f64(), x * y));
        }

        #[test]
        fn test_div(x in prop::num::f64::ANY, y in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).div(&APFloat::from_f64(y));
            prop_assert!(f64_eq(res.to_f64(), x / y));
        }

        #[test]
        fn test_sqrt(x in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).sqrt();
            prop_assert!(f64_eq(res.to_f64(), x.sqrt()));
        }

        #[test]
        fn test_fma(x in prop::num::f64::ANY, y in prop::num::f64::ANY, z in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).fma(&APFloat::from_f64(y), &APFloat::from_f64(z));
            prop_assert!(f64_eq(res.to_f64(), x.mul_add(y, z)));
        }

        #[test]
        fn test_neg(x in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).neg();
            prop_assert!(f64_eq(res.to_f64(), -x));
        }

        #[test]
        fn test_abs(x in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).abs();
            prop_assert!(f64_eq(res.to_f64(), x.abs()));
        }

        #[test]
        fn test_compare(x in prop::num::f64::ANY, y in prop::num::f64::ANY) {
            let res = APFloat::from_f64(x).compare(&APFloat::from_f64(y));
            prop_assert_eq!(res, x.partial_cmp(&y));
        }
    }
}
