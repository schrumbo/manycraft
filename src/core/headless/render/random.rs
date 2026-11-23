//! The game's position seeds and its legacy random generator, so weighted models and plant offsets land as in the game.

/// The seed the game derives from a block position.
pub fn seed(x: i32, y: i32, z: i32) -> i64 {
    let mut l = i64::from(x.wrapping_mul(3_129_871)) ^ i64::from(z).wrapping_mul(116_129_781) ^ i64::from(y);
    l = l.wrapping_mul(l).wrapping_mul(42_317_861).wrapping_add(l.wrapping_mul(11));
    l >> 16
}

const MULTIPLIER: i64 = 0x5_DEEC_E66D;
const MASK: i64 = (1 << 48) - 1;

/// The linear congruential generator the game uses for models.
pub struct Legacy(i64);

impl Legacy {
    pub fn new(seed: i64) -> Self {
        Self((seed ^ MULTIPLIER) & MASK)
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.0 = (self.0.wrapping_mul(MULTIPLIER).wrapping_add(11)) & MASK;
        (self.0 >> (48 - bits)) as i32
    }

    /// A full 64-bit number, as `nextLong()`.
    pub fn next_float(&mut self) -> f32 {
        self.next(24) as f32 / (1 << 24) as f32
    }

    pub fn next_double(&mut self) -> f64 {
        ((i64::from(self.next(26)) << 27) + i64::from(self.next(27))) as f64 / (1i64 << 53) as f64
    }

    pub fn next_long(&mut self) -> i64 {
        (i64::from(self.next(32)) << 32).wrapping_add(i64::from(self.next(32)))
    }

    /// A number in `0..bound`, as `nextInt(bound)`.
    pub fn next_int(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        if bound & -bound == bound {
            return ((i64::from(bound) * i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let value = bits % bound;
            if bits - value + (bound - 1) >= 0 {
                return value;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_java_util_random() {
        let mut r = Legacy::new(42);
        assert_eq!([r.next_int(10), r.next_int(10), r.next_int(10)], [0, 3, 8]);
        let mut p = Legacy::new(0);
        assert_eq!(p.next_int(4), 2);
    }

    #[test]
    fn position_seeds_are_stable() {
        assert_eq!(seed(0, 0, 0), 0);
        assert_eq!(seed(1, 64, -3), -62_687_207_132_611);
        assert_ne!(seed(1, 64, -3), seed(-3, 64, 1));
    }
}
