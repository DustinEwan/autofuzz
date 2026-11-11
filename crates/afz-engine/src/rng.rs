//! The scan's one random source.
//!
//! One 32-bit seed drives every random choice in a scan: corpus selection, mutation, and
//! value generation. That is what makes `--seed` replay mean something, so this module owns
//! the only path to randomness in the engine, and it deliberately exposes only
//! `next_u32` and `next_u64` from `rand`.
//!
//! `rand::distributions` is not used anywhere in this crate. Its algorithms are not guaranteed
//! stable across releases, and a change would silently break seed replay, which is the
//! reproducibility guarantee the scan result records.

use rand::Rng as _;
use rand::SeedableRng as _;
use rand_chacha::ChaCha8Rng;

/// The engine's random source. `ChaCha8Rng` is deterministic for a given seed on every
/// platform, which a `ThreadRng` is not.
#[derive(Debug, Clone)]
pub struct Rng {
    inner: ChaCha8Rng,
    /// The seed this stream was created from, kept so [`Rng::fork`] can derive a worker stream
    /// without consuming draws from this one.
    seed: u64,
}

impl Rng {
    /// Seed from the scan's 32-bit seed. The scan result records this value, and replaying
    /// `--seed` with it must reproduce the same invocation sequence.
    pub fn seeded(seed: u32) -> Rng {
        Rng::seeded_u64(u64::from(seed))
    }

    /// Seed from a full 64-bit value, for a stream that must not collide with another worker's
    /// 32-bit seed.
    pub fn seeded_u64(seed: u64) -> Rng {
        Rng { inner: ChaCha8Rng::seed_from_u64(seed), seed }
    }

    /// The seed this stream was derived from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Derive an independent stream for a worker, so the worker count does not change what any
    /// single worker draws.
    ///
    /// The derivation is a pure function of `(seed, worker)` and draws nothing from this
    /// stream: a worker restarted mid-scan, or a scan run with a different `--workers`, must
    /// still produce the same per-worker sequence for the same scan seed.
    pub fn fork(&self, worker: u32) -> Rng {
        Rng::seeded_u64(derive(self.seed, worker))
    }

    pub fn next_u32(&mut self) -> u32 {
        self.inner.next_u32()
    }

    pub fn next_u64(&mut self) -> u64 {
        self.inner.next_u64()
    }

    /// Uniform in `[0, bound)`. Modulo bias is irrelevant at these bounds and keeps the
    /// derivation independent of `rand`'s distribution code.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_u64() % bound
        }
    }

    /// Uniform in `[low, high]`. Returns `low` when the range is empty.
    pub fn between(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            low
        } else {
            low + self.below(high - low + 1)
        }
    }

    /// True with probability `numerator / denominator`.
    pub fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        denominator > 0 && self.below(denominator) < numerator
    }

    /// A float in `[0, 1)`, drawn from 53 bits so the value is exactly representable.
    pub fn unit(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        bits as f64 / (1u64 << 53) as f64
    }

    pub fn boolean(&mut self) -> bool {
        self.next_u32() & 1 == 1
    }

    /// One element of `items`, or `None` when it is empty.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        items.get(self.below(items.len() as u64) as usize)
    }

    /// A printable ASCII character. Strings stay ASCII so a JSON artifact is readable in a
    /// triage queue.
    pub fn ascii_char(&mut self) -> char {
        const FIRST: u8 = 33;
        const LAST: u8 = 126;
        char::from(FIRST + self.below((LAST - FIRST + 1) as u64) as u8)
    }

    /// A lowercase letter, used where a target is likely to expect an identifier.
    pub fn lowercase(&mut self) -> char {
        char::from(b'a' + self.below(26) as u8)
    }

    /// A finite `f64` in `[min, max]`. Non-finite bounds are clamped to the finite extremes,
    /// because a range that contains infinity has no uniform draw.
    pub fn float_in(&mut self, min: f64, max: f64) -> f64 {
        let lo = if min.is_finite() { min } else { -1e308 };
        let hi = if max.is_finite() { max } else { 1e308 };
        if hi <= lo {
            return lo;
        }
        lo + self.unit() * (hi - lo)
    }

    /// An integer in `[min, max]`, drawn through `next_u64` so the sequence is stable.
    ///
    /// The offset is added in `i128`: narrowing it to `i64` first would wrap for a full-span
    /// range, and `i64::MIN + i64::MIN` panics on overflow.
    pub fn int_in(&mut self, min: i64, max: i64) -> i64 {
        if max <= min {
            return min;
        }
        let span = max as i128 - min as i128 + 1;
        let offset = self.next_u64() as i128 % span;
        (min as i128 + offset) as i64
    }
}

/// Mix a scan seed with a worker index. The two constants are the splitmix64 multipliers, and
/// the mixing lives here rather than in `rand` so a `rand` upgrade cannot change which stream
/// a given `(seed, worker)` pair produces.
fn derive(seed: u64, worker: u32) -> u64 {
    let mut z = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(u64::from(worker).wrapping_mul(0xBF58_476D_1CE4_E5B9));
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 27;
    z.wrapping_mul(0x94D0_49BB_1331_11EB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_draws_the_same_sequence() {
        let mut a = Rng::seeded(0x5e3a91c4);
        let mut b = Rng::seeded(0x5e3a91c4);
        for _ in 0..64 {
            assert_eq!(a.next_u32(), b.next_u32());
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::seeded(1).next_u32(), Rng::seeded(2).next_u32());
    }

    #[test]
    fn a_fork_is_a_pure_function_of_the_seed_and_worker() {
        let parent = Rng::seeded(0x5e3a91c4);
        assert_ne!(parent.fork(0).next_u64(), parent.fork(1).next_u64(), "two workers share a stream");
        // Forking must not consume draws, or worker 1 would see a different stream depending on
        // how many workers were forked before it.
        let mut before = parent.clone();
        let first = before.next_u64();
        let mut after = parent.clone();
        for worker in 0..8 {
            let _ = parent.fork(worker);
        }
        assert_eq!(after.next_u64(), first, "a fork disturbed the parent stream");
        assert_eq!(parent.fork(3).next_u64(), Rng::seeded(0x5e3a91c4).fork(3).next_u64());
    }

    #[test]
    fn derived_draws_stay_in_range() {
        let mut rng = Rng::seeded(7);
        for _ in 0..10_000 {
            assert!(rng.below(10) < 10);
            assert_eq!(rng.below(0), 0);
            assert!((3..=7).contains(&rng.between(3, 7)));
            assert_eq!(rng.between(5, 5), 5);
            let u = rng.unit();
            assert!((0.0..1.0).contains(&u));
            let f = rng.float_in(-2.0, 3.0);
            assert!((-2.0..=3.0).contains(&f), "{f}");
            let i = rng.int_in(-5, 5);
            assert!((-5..=5).contains(&i), "{i}");
        }
        assert_eq!(rng.pick::<u8>(&[]), None);
        assert!(rng.chance(0, 100).not());
        assert!(rng.chance(100, 100));
        assert!(!rng.chance(1, 0));
    }

    #[test]
    fn int_in_spans_zero_when_the_range_does() {
        // The risk this guards is a signed-width bug that pins every draw to one side of zero,
        // which a bare bounds assertion cannot see.
        let mut rng = Rng::seeded(11);
        let (mut negative, mut positive) = (0, 0);
        for _ in 0..1000 {
            let v = rng.int_in(i64::MIN, i64::MAX);
            if v < 0 {
                negative += 1;
            } else {
                positive += 1;
            }
            assert!((0..=100).contains(&rng.int_in(0, 100)));
        }
        assert!(negative > 100 && positive > 100, "full-span draws: {negative} negative, {positive} positive");
        assert_eq!(rng.int_in(i64::MAX, i64::MIN), i64::MAX);
    }

    /// The guard against the real risk: a `rand` upgrade that changes the sequence would make
    /// a recorded seed stop reproducing. These are the first draws of seed `0x5e3a91c4`.
    #[test]
    fn the_pinned_sequence_is_recorded() {
        // Verified against `rand_chacha::ChaCha8Rng` directly, outside this wrapper, so the
        // numbers below are the library's sequence and not a restatement of this code.
        let mut rng = Rng::seeded(0x5e3a91c4);
        assert_eq!(rng.next_u32(), 3_476_150_232);
        assert_eq!(rng.next_u32(), 189_151_496);
        assert_eq!(rng.next_u64(), 8_317_733_627_508_850_721);
        assert_eq!(Rng::seeded(0x5e3a91c4).next_u64(), 812_399_492_785_625_048);
    }

    trait Not {
        fn not(self) -> bool;
    }
    impl Not for bool {
        fn not(self) -> bool {
            !self
        }
    }
}
