//! What tests of more than one feature share.

/// Picks that come out the same on every run, for tests that try many cases.
pub struct Picks(u64);

impl Picks {
    pub fn seeded() -> Self {
        Self(0x2545_f491_4f6c_dd1d)
    }

    /// A number below `below`.
    pub fn below(&mut self, below: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % u64::try_from(below).unwrap()).unwrap()
    }

    /// Up to `most` of `parts` one after another, and the same bytes cut anywhere into three
    /// pieces, some of which may be empty.
    pub fn pieces(&mut self, parts: &[&[u8]], most: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let count = self.below(most + 1);
        let whole: Vec<u8> = (0..count)
            .flat_map(|_| parts[self.below(parts.len())].to_vec())
            .collect();
        let mut cuts = [self.below(whole.len() + 1), self.below(whole.len() + 1)];
        cuts.sort_unstable();
        let pieces = [
            &whole[..cuts[0]],
            &whole[cuts[0]..cuts[1]],
            &whole[cuts[1]..],
        ]
        .map(<[u8]>::to_vec)
        .to_vec();
        (whole, pieces)
    }
}
