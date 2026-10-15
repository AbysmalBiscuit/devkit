//! Readable names for todos: the word name a uuid id carries, and the
//! resolver that reads a typed name back to its todo.
//!
//! The word list is drawn from the EFF large wordlist
//! (<https://www.eff.org/dice>, CC BY 3.0 US): its words of 3 to 7 lowercase
//! letters, less every word spelled only with hex letters, sorted, the first
//! 4096.

use std::sync::LazyLock;

/// The words a word name is drawn from, 4096 of them, so 12 bits pick one.
pub static WORDS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| include_str!("words.txt").lines().collect());

/// The word name of the todo whose id is `id`: two words picked by the
/// uuid's last 24 bits, joined by `-`. `None` for an id that is not a uuid.
pub fn word_name(id: &str) -> Option<String> {
    if id.len() != 36 {
        return None;
    }
    let bytes = uuid::Uuid::try_parse(id).ok()?.into_bytes();
    let bits = u32::from_be_bytes([0, bytes[13], bytes[14], bytes[15]]) as usize;
    Some(format!("{}-{}", WORDS[bits >> 12], WORDS[bits & 0xfff]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_holds_4096_distinct_words_none_of_them_hex() {
        assert_eq!(WORDS.len(), 4096);
        assert_eq!(
            WORDS.iter().collect::<std::collections::HashSet<_>>().len(),
            4096
        );
        assert!(
            WORDS
                .iter()
                .all(|w| (3..=7).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase()))
        );
        assert!(
            WORDS
                .iter()
                .all(|w| !w.bytes().all(|b| (b'a'..=b'f').contains(&b)))
        );
    }

    #[test]
    fn a_uuid_names_itself_from_its_last_24_bits() {
        let id = "96432cd6-082a-4d8c-a9cb-adef8823ff92";
        // The last 24 bits are 0x23ff92: 0x23f picks the first word, 0xf92
        // the second.
        assert_eq!(
            word_name(id),
            Some(format!("{}-{}", WORDS[0x23f], WORDS[0xf92]))
        );
        assert_eq!(word_name(&id.to_uppercase()), word_name(id));
    }

    #[test]
    fn an_id_that_is_not_a_uuid_has_no_word_name() {
        assert_eq!(word_name("65"), None);
        assert_eq!(word_name("96432cd6"), None);
        assert_eq!(word_name("96432cd6082a4d8ca9cbadef8823ff92"), None);
    }
}
