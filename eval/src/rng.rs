//! A seeded splitmix64 — the whole harness has to be reproducible from a
//! single `--seed`, and a dependency for eight lines of arithmetic isn't
//! worth the CI build time.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-enough in `0..n`; `n == 0` yields 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    /// Fisher-Yates. Used to permute the slot vocabularies per seed so the
    /// mixed-radix fact enumeration still varies run to run.
    pub fn shuffle<T>(&mut self, xs: &mut [T]) {
        for i in (1..xs.len()).rev() {
            xs.swap(i, self.below(i + 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn same_seed_same_stream() {
        let a: Vec<u64> = (0..8).map(|_| Rng::new(7).next_u64()).collect();
        let mut r = Rng::new(7);
        let b: Vec<u64> = (0..8).map(|_| r.next_u64()).collect();
        assert_eq!(a[0], b[0]);
        assert_ne!(b[0], b[1], "stream must advance");

        let mut x = Rng::new(1);
        let mut y = Rng::new(1);
        assert_eq!(x.next_u64(), y.next_u64());
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut v: Vec<usize> = (0..64).collect();
        Rng::new(3).shuffle(&mut v);
        let mut sorted = v.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..64).collect::<Vec<_>>());
        assert_ne!(v, sorted, "a 64-element shuffle should move something");
    }

    #[test]
    fn below_stays_in_range() {
        let mut r = Rng::new(11);
        for _ in 0..200 {
            assert!(r.below(5) < 5);
        }
        assert_eq!(r.below(0), 0);
    }
}
