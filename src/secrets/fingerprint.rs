//! Fingerprints: 128 bits of a keyed hash. Under one key the same text gives the same print,
//! however it's fed, and without the key, which is never shown, no text can be made to give
//! another's.

use std::collections::hash_map::{DefaultHasher, RandomState};
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

/// What tells one text from another under a key. A print of only part of a text is never equal
/// to a whole one.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    value: u128,
    partial: bool,
}

impl Fingerprint {
    /// Whether it was made of only part of what it stands for, such as a private key that a hunk
    /// cuts off before its end.
    pub fn is_partial(self) -> bool {
        self.partial
    }

    /// A partial print that no other is equal to, for a secret whose text a scan never had all of.
    pub fn unmatched() -> Self {
        static MADE: AtomicU64 = AtomicU64::new(0);
        let mut print = Key::for_run().start();
        print.feed(b"unmatched ");
        print.feed(&MADE.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        print.finish_partial()
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.partial {
            "Fingerprint(partial, …)"
        } else {
            "Fingerprint(…)"
        })
    }
}

/// A key to make fingerprints with.
pub struct Key(RandomState);

/// The key a run makes the fingerprints of secrets with, so that what one scan finds can be told
/// from what another found. Drawing it stops the run rather than fall back on a key drawn here.
static RUN: LazyLock<Key> = LazyLock::new(|| {
    let drawn = thread::spawn(RandomState::new).join();
    Key(drawn.expect("a key for fingerprints, drawn on a thread of its own"))
});

impl Key {
    /// The key of the run, drawn once, on a thread of its own, so that it shares nothing with the
    /// keys drawn on this one.
    pub fn for_run() -> &'static Key {
        &RUN
    }

    /// A fingerprint to feed text as it comes. Fed in pieces, it's the same as fed whole, since
    /// the hasher joins what it's fed, as std's does; if it didn't, a secret would be listed
    /// twice, never hidden.
    pub fn start(&self) -> Fingerprinting {
        Fingerprinting([0, 1].map(|part| {
            let mut hasher = self.0.build_hasher();
            hasher.write_u8(part);
            hasher
        }))
    }

    /// The fingerprint of `bytes`.
    pub fn of(&self, bytes: &[u8]) -> Fingerprint {
        let mut print = self.start();
        print.feed(bytes);
        print.finish()
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(…)")
    }
}

/// A fingerprint being fed, in its two halves.
#[derive(Clone)]
pub struct Fingerprinting([DefaultHasher; 2]);

impl Fingerprinting {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.0.iter_mut().for_each(|half| half.write(bytes));
    }

    /// The print of all that was fed.
    pub fn finish(&self) -> Fingerprint {
        Fingerprint {
            value: self.value(),
            partial: false,
        }
    }

    /// The print of what was fed, as only part of what it stands for.
    pub fn finish_partial(&self) -> Fingerprint {
        Fingerprint {
            value: self.value(),
            partial: true,
        }
    }

    fn value(&self) -> u128 {
        let [high, low] = &self.0;
        (u128::from(high.finish()) << 64) | u128::from(low.finish())
    }
}

impl fmt::Debug for Fingerprinting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Fingerprinting(…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_gives_the_same_print_however_it_is_fed() {
        let key = Key::for_run();
        let mut pieces = key.start();
        pieces.feed(b"AKIA");
        pieces.feed(b"");
        pieces.feed(b"QQQQ");
        assert_eq!(pieces.finish(), key.of(b"AKIAQQQQ"));
        assert_ne!(key.of(b"AKIAQQQQ"), key.of(b"AKIAQQQR"));
        assert_eq!(Key::for_run().of(b"x"), key.of(b"x"));
    }

    #[test]
    fn a_partial_print_is_never_a_whole_one() {
        let mut print = Key::for_run().start();
        print.feed(b"-----BEGIN");
        let (whole, partial) = (print.finish(), print.finish_partial());
        assert_ne!(whole, partial);
        assert!(partial.partial && !whole.partial);
        assert_ne!(Fingerprint::unmatched(), Fingerprint::unmatched());
        assert!(Fingerprint::unmatched().partial);
    }

    #[test]
    fn the_run_has_one_key() {
        assert!(std::ptr::eq(Key::for_run(), Key::for_run()));
        let other = Key(RandomState::new());
        assert_ne!(Key::for_run().of(b"same"), other.of(b"same"));
    }

    #[test]
    fn shows_nothing_of_the_key_or_the_text() {
        let mut print = Key::for_run().start();
        print.feed(b"secret-text");
        let shown = format!(
            "{:?} {:?} {:?} {:?}",
            print,
            print.finish(),
            print.finish_partial(),
            Key::for_run()
        );
        assert_eq!(
            shown,
            "Fingerprinting(…) Fingerprint(…) Fingerprint(partial, …) Key(…)"
        );
    }
}
