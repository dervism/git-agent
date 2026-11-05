//! Text that can hold any bytes, so a patch survives being worked on as text.
//!
//! A file that isn't UTF-8, such as one in Latin-1, must reach `git apply` byte
//! for byte. Bytes that aren't valid UTF-8 are held as private characters
//! instead, and turned back at the end. What people and models read gets
//! U+FFFD in their place, through [`readable`].

/// The private character for byte 0x80; those for higher bytes follow it.
const FIRST_PRIVATE: u32 = 0x10ef80;
const FIRST_BYTE: u32 = 0x80;

/// A byte as a private character, for the bytes that UTF-8 can leave invalid.
fn private_character(byte: u8) -> char {
    char::from_u32(FIRST_PRIVATE + (u32::from(byte) - FIRST_BYTE)).unwrap_or('\u{fffd}')
}

/// The byte a private character holds, if `character` is one.
fn held_byte(character: char) -> Option<u8> {
    let offset = u32::from(character).checked_sub(FIRST_PRIVATE)?;
    u8::try_from(offset + FIRST_BYTE).ok()
}

/// `bytes` as text that [`to_bytes`] turns back exactly. A character already
/// in the private range is held byte by byte too, so nothing can collide.
pub fn to_text(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for character in chunk.valid().chars() {
            if held_byte(character).is_some() {
                let mut encoded = [0; 4];
                let held = character.encode_utf8(&mut encoded).bytes();
                text.extend(held.map(private_character));
            } else {
                text.push(character);
            }
        }
        text.extend(chunk.invalid().iter().copied().map(private_character));
    }
    text
}

/// The bytes that `text` from [`to_text`] holds.
pub fn to_bytes(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len());
    for character in text.chars() {
        match held_byte(character) {
            Some(byte) => bytes.push(byte),
            None => bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    bytes
}

/// `text` from [`to_text`] for people and models to read: bytes that aren't
/// valid UTF-8 become U+FFFD, as they do in git's other output.
pub fn readable(text: &str) -> String {
    text.chars()
        .map(|character| match held_byte(character) {
            Some(_) => '\u{fffd}',
            None => character,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(bytes: &[u8]) {
        assert_eq!(to_bytes(&to_text(bytes)), bytes, "{bytes:?}");
    }

    #[test]
    fn leaves_valid_text_as_it_is() {
        let text = "blåbærsyltetøy 日本語 \u{1f600}\n";
        assert_eq!(to_text(text.as_bytes()), text);
        assert_eq!(to_bytes(text), text.as_bytes());
        assert_eq!(readable(text), text);
    }

    #[test]
    fn keeps_every_byte_value() {
        let every_byte: Vec<u8> = (0..=255).collect();
        round_trip(&every_byte);
        for byte in 0..=255 {
            round_trip(&[byte]);
            round_trip(&[b'a', byte, b'z']);
            round_trip(&[0xc3, byte]);
            round_trip(&[0xf4, 0x8e, 0xbe, byte]);
        }
    }

    #[test]
    fn keeps_latin1_text_and_shows_a_placeholder() {
        let latin1 = b"navn=Kj\xf8re\n";
        let text = to_text(latin1);
        assert_eq!(to_bytes(&text), latin1);
        assert_eq!(readable(&text), "navn=Kj\u{fffd}re\n");
    }

    #[test]
    fn keeps_a_cut_character_at_the_end() {
        round_trip("æ".as_bytes().split_last().unwrap().1);
        round_trip(&"日".as_bytes()[..2]);
    }

    #[test]
    fn holds_real_private_characters_byte_by_byte() {
        for character in ['\u{10ef7f}', '\u{10ef80}', '\u{10efff}', '\u{10f000}'] {
            let text = character.to_string();
            round_trip(text.as_bytes());
        }
        let private = "\u{10efb8}";
        assert_ne!(to_text(private.as_bytes()), private);
        assert_eq!(readable(&to_text(private.as_bytes())), "\u{fffd}".repeat(4));
    }

    #[test]
    fn handles_no_bytes() {
        round_trip(&[]);
        assert_eq!(to_text(&[]), "");
    }
}
