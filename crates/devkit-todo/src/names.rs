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
/// key matches the key a todo's ticket carries, and an issue URL matches the
/// GitHub key its path spells. A GitHub short form (`#65`, `65`) is a key
/// only when `tracker_key` reads it into one. `None` when nothing matches,
/// and an error naming each match when more than one todo does.
pub fn resolve(
    input: &str,
    todos: Vec<Todo>,
    tracker_key: Option<&TrackerKey>,
) -> anyhow::Result<Option<Todo>> {
    let lower = input.to_ascii_lowercase();
    let github_key = issue_url_key(input).or_else(|| {
        tracker_key
            .filter(|_| is_github_short_form(input))
            .and_then(|read| read(input))
    });
    let hex = lower.len() >= 4 && lower.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit());
    let by_key = |ticket: &TicketRef| {
        ticket.key.eq_ignore_ascii_case(input)
            || ticket.tracker == "github"
                && github_key
                    .as_deref()
                    .is_some_and(|k| k.eq_ignore_ascii_case(&ticket.key))
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

/// Whether `input` is a GitHub issue's short form, `#65` or `65`, which only
/// the caller's tracker reads into a full key.
pub fn is_github_short_form(input: &str) -> bool {
    let number = input.strip_prefix('#').unwrap_or(input);
    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
}

/// The key `owner/repo#65` that an issue URL on any host,
/// `https://<host>/<owner>/<repo>/issues/<n>`, names.
fn issue_url_key(input: &str) -> Option<String> {
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))?;
    let mut parts = rest.split('/').skip(1);
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    if parts.next()? != "issues" {
        return None;
    }
    let tail = parts.next()?;
    let digits = &tail[..tail.bytes().take_while(u8::is_ascii_digit).count()];
    let number: u64 = digits.parse().ok()?;
    Some(format!("{owner}/{repo}#{number}"))
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
    fn a_number_without_a_github_tracker_is_not_a_key() {
        assert_eq!(id(resolve("65", tickets(), None)), None);
        assert_eq!(id(resolve("#65", tickets(), None)), None);
    }

    #[test]
    fn an_issue_url_resolves_without_a_tracker() {
        let url = "https://github.com/O/R/issues/65";
        assert_eq!(id(resolve(url, tickets(), None)), Some(A.into()));
        let comment = "http://ghe.example/x/y/issues/65#issuecomment-1";
        assert_eq!(id(resolve(comment, tickets(), None)), Some(C.into()));
        for other in [
            "https://github.com/o/r/pull/65",
            "https://github.com/o/r/issues/",
            "https://github.com/o/issues/65",
        ] {
            assert_eq!(id(resolve(other, tickets(), None)), None, "{other}");
        }
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
        let tracker = |input: &str| (input == "1234").then(|| "o/r#1234".to_string());
        let err = resolve("1234", todos, Some(&tracker))
            .unwrap_err()
            .to_string();
        assert_eq!(err, "todo name 1234 is ambiguous: 1234abcd, 96432cd6");
    }

    #[test]
    fn a_cancelled_todo_whose_text_is_the_key_is_not_matched() {
        let mut stale: Todo = todo(D, None);
        stale.description = "#65".into();
        stale.status = crate::Status::Cancelled { by: None };
        let todos = vec![stale, todo(A, Some(("github", "o/r#65")))];
        let tracker = |_: &str| Some("o/r#65".to_string());
        assert_eq!(id(resolve("#65", todos, Some(&tracker))), Some(A.into()));
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
