//! Readable names for todos: the word name a uuid id carries, and the
//! resolver that reads a typed name back to its todo.
//!
//! The word list is drawn from the EFF large wordlist
//! (<https://www.eff.org/dice>, CC BY 3.0 US): its words of 3 to 7 lowercase
//! letters, less every word spelled only with hex letters, sorted, the first
//! 4096.

use std::sync::LazyLock;

use crate::{TicketRef, Todo};

/// The words a word name is drawn from, 4096 of them, so 12 bits pick one.
pub static WORDS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| include_str!("words.txt").lines().collect());

/// The word name of the todo whose id is `id`: two words picked by the
/// uuid's last 24 bits, joined by `-`. `None` for an id that is not a uuid.
pub fn word_name(id: &str) -> Option<String> {
    let bytes = uuid(id)?.into_bytes();
    let bits = u32::from_be_bytes([0, bytes[13], bytes[14], bytes[15]]) as usize;
    Some(format!("{}-{}", WORDS[bits >> 12], WORDS[bits & 0xfff]))
}

/// `id` as a uuid, when it is one in the hyphenated form [`crate::short_id`]
/// shortens.
fn uuid(id: &str) -> Option<uuid::Uuid> {
    (id.len() == 36).then(|| uuid::Uuid::try_parse(id).ok())?
}

/// Reads a GitHub issue's short form into the full ticket key the caller's
/// tracker gives it, or `None` for input that tracker refuses.
pub type TrackerKey = dyn Fn(&str) -> Option<String>;

/// Whether the ticket key `key` names the tracker issue `issue`:
/// `SWE-123` names itself, and `owner/repo#65` is issue `65`.
pub fn names_issue(key: &str, issue: &str) -> bool {
    key.eq_ignore_ascii_case(issue) || key.rsplit_once('#').is_some_and(|(_, n)| n == issue)
}

/// The todo in `todos` that `input` names, read as a ticket key, a word name
/// or a uuid prefix of 4 or more hex characters, all ignoring case. A full
/// key matches the key a todo's ticket carries; a GitHub short form (`#65`,
/// `65`, an issue URL) is read by `tracker_key` into a full key, or with no
/// tracker matches every GitHub ticket of that number. `None` when nothing
/// matches, and an error naming each match when more than one todo does.
pub fn resolve(
    input: &str,
    todos: Vec<Todo>,
    tracker_key: Option<&TrackerKey>,
) -> anyhow::Result<Option<Todo>> {
    let lower = input.to_ascii_lowercase();
    let short = github_short_form(input);
    let read = short.and(tracker_key).and_then(|read| read(input));
    let hex = lower.len() >= 4 && lower.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit());
    let by_key = |ticket: &TicketRef| {
        ticket.key.eq_ignore_ascii_case(input)
            || ticket.tracker == "github"
                && match (short, tracker_key) {
                    (Some(_), Some(_)) => read
                        .as_deref()
                        .is_some_and(|k| k.eq_ignore_ascii_case(&ticket.key)),
                    (Some(Some(number)), None) => names_issue(&ticket.key, number),
                    _ => false,
                }
    };
    let names = |todo: &Todo| {
        todo.ticket.as_ref().is_some_and(by_key)
            || uuid(&todo.id).is_some_and(|_| {
                word_name(&todo.id).is_some_and(|w| w == lower)
                    || (hex && todo.id.to_ascii_lowercase().starts_with(&lower))
            })
    };
    let mut found: Vec<Todo> = todos.into_iter().filter(names).collect();
    if found.len() < 2 {
        return Ok(found.pop());
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<&str> = found.iter().map(|t| crate::short_id(&t.id)).collect();
    anyhow::bail!("todo name {input} is ambiguous: {}", ids.join(", "))
}

/// Whether `input` is a GitHub issue's short form, `#65`, `65` or an issue
/// URL, which only the caller's tracker reads into a full key.
pub fn is_github_short_form(input: &str) -> bool {
    github_short_form(input).is_some()
}

/// Whether `input` is a GitHub issue's short form, `#65`, `65` or an issue
/// URL, with the number it carries when it carries one without a tracker's
/// reading.
fn github_short_form(input: &str) -> Option<Option<&str>> {
    let number = input.strip_prefix('#').unwrap_or(input);
    if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
        Some(Some(number))
    } else if input.starts_with("https://") || input.starts_with("http://") {
        Some(None)
    } else {
        None
    }
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

    fn todo(id: &str, ticket: Option<(&str, &str)>) -> Todo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "description": "a todo",
            "status": "pending",
            "ticket": ticket.map(|(tracker, key)| serde_json::json!({"tracker": tracker, "key": key})),
        }))
        .unwrap()
    }

    const A: &str = "96432cd6-082a-4d8c-a9cb-adef8823ff92";
    const B: &str = "1234abcd-0000-4000-8000-000000000001";
    const C: &str = "5c0ffee0-0000-4000-8000-000000000002";
    const D: &str = "77777777-0000-4000-8000-000000000003";

    fn tickets() -> Vec<Todo> {
        vec![
            todo(A, Some(("github", "o/r#65"))),
            todo(B, Some(("linear", "SWE-123"))),
            todo(C, Some(("github", "x/y#65"))),
            todo(D, None),
        ]
    }

    fn id(found: anyhow::Result<Option<Todo>>) -> Option<String> {
        found.unwrap().map(|t| t.id)
    }

    #[test]
    fn a_full_key_resolves_ignoring_case() {
        assert_eq!(id(resolve("swe-123", tickets(), None)), Some(B.into()));
        assert_eq!(id(resolve("O/R#65", tickets(), None)), Some(A.into()));
    }

    #[test]
    fn a_number_matches_every_github_ticket_without_a_tracker() {
        let err = resolve("65", tickets(), None).unwrap_err().to_string();
        assert!(
            err.contains("96432cd6") && err.contains("5c0ffee0"),
            "{err}"
        );
        let err = resolve("#65", tickets(), None).unwrap_err().to_string();
        assert!(
            err.contains("96432cd6") && err.contains("5c0ffee0"),
            "{err}"
        );
    }

    #[test]
    fn a_tracker_reads_the_short_form() {
        let tracker = |input: &str| ["#65", "65"].contains(&input).then(|| "o/r#65".to_string());
        assert_eq!(
            id(resolve("#65", tickets(), Some(&tracker))),
            Some(A.into())
        );
        assert_eq!(id(resolve("65", tickets(), Some(&tracker))), Some(A.into()));
    }

    #[test]
    fn a_refused_short_form_is_not_a_key() {
        let refuses = |_: &str| None;
        assert_eq!(id(resolve("65", tickets(), Some(&refuses))), None);
    }

    #[test]
    fn a_word_name_resolves_in_any_case() {
        let name = word_name(D).unwrap();
        assert_eq!(id(resolve(&name, tickets(), None)), Some(D.into()));
        assert_eq!(
            id(resolve(&name.to_uppercase(), tickets(), None)),
            Some(D.into())
        );
        let ticket_name = word_name(A).unwrap();
        assert_eq!(id(resolve(&ticket_name, tickets(), None)), Some(A.into()));
    }

    #[test]
    fn a_four_char_prefix_resolves_and_three_does_not() {
        assert_eq!(id(resolve("7777", tickets(), None)), Some(D.into()));
        assert_eq!(id(resolve("5C0F", tickets(), None)), Some(C.into()));
        assert_eq!(id(resolve("777", tickets(), None)), None);
        let builtin = vec![todo("65", None), todo("6512", None)];
        assert_eq!(id(resolve("6512", builtin, None)), None);
    }

    #[test]
    fn a_match_across_forms_is_ambiguous() {
        let todos = vec![todo(B, None), todo(A, Some(("github", "o/r#1234")))];
        let err = resolve("1234", todos, None).unwrap_err().to_string();
        assert_eq!(err, "todo name 1234 is ambiguous: 1234abcd, 96432cd6");
    }

    #[test]
    fn a_cancelled_todo_whose_text_is_the_key_is_not_matched() {
        let mut stale: Todo = todo(D, None);
        stale.description = "#65".into();
        stale.status = crate::Status::Cancelled { by: None };
        let todos = vec![stale, todo(A, Some(("github", "o/r#65")))];
        assert_eq!(id(resolve("#65", todos, None)), Some(A.into()));
    }

    #[test]
    fn a_closed_todo_resolves() {
        let mut done = todo(A, Some(("github", "o/r#65")));
        done.status = crate::Status::Completed { by: None };
        assert_eq!(id(resolve("o/r#65", vec![done], None)), Some(A.into()));
    }

    #[test]
    fn blank_input_names_nothing() {
        assert_eq!(id(resolve("", tickets(), None)), None);
        assert_eq!(id(resolve("  ", tickets(), None)), None);
        assert_eq!(id(resolve("#", tickets(), None)), None);
        assert_eq!(id(resolve("----", tickets(), None)), None);
    }

    #[test]
    fn a_ticket_key_names_its_issue() {
        assert!(names_issue("SWE-123", "swe-123"));
        assert!(names_issue("o/r#65", "65"));
        assert!(!names_issue("o/r#65", "6"));
        assert!(!names_issue("SWE-123", "SWE-12"));
    }
}
