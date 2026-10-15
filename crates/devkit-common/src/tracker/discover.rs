//! What `ticket fields discover` read from a tracker: each place a ticket
//! field can be written, with its type and values, under the field name it
//! is added as.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use devkit_config::{EnumValue, FieldType, Place};
use serde_json::Value as Json;

/// A tracker's ticket fields, by the name each is added as.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Discovered {
    pub fields: BTreeMap<String, DiscoveredField>,
    /// Tracker users no `[people]` entry names, as `<login or id> (<name>)`.
    pub unmatched_users: Vec<String>,
    /// The `[people]` key that would name each of `unmatched_users`.
    pub people_key: &'static str,
    /// Lists the tracker answered only in part, said in a sentence each.
    pub truncated: Vec<String>,
    /// Fields someone created on the tracker whose type devkit cannot write,
    /// as `` `<name>` (<type>) ``.
    pub unwritable: Vec<String>,
    /// The keys of the Linear teams the read covered, whose team-bound values
    /// it answered in full; empty when the tracker has no teams.
    pub teams_read: Vec<String>,
}

/// One place on the tracker and what it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredField {
    pub r#type: FieldType,
    pub list: bool,
    pub r#where: Place,
    /// For an `enum`, every value the tracker offers, in its order.
    pub values: Vec<EnumValue>,
    /// The tracker answered only part of `values`, so a declared value
    /// missing from them may still be offered.
    pub partial: bool,
}

impl DiscoveredField {
    pub fn new(r#type: FieldType, r#where: Place) -> Self {
        DiscoveredField {
            r#type,
            list: false,
            r#where,
            values: Vec::new(),
            partial: false,
        }
    }

    pub fn list(mut self) -> Self {
        self.list = true;
        self
    }

    pub fn with_values(mut self, values: Vec<EnumValue>) -> Self {
        self.values = values;
        self
    }

    /// Mark `values` as read only in part when `partial`.
    pub fn partly(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }
}

/// A field name for a tracker field called `name`: lowercase, with each run
/// of other characters as one `_`.
pub fn field_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// An enum value whose name and id are both `name`.
pub fn named(name: &str) -> EnumValue {
    EnumValue {
        name: name.to_string(),
        id: name.to_string(),
        teams: Vec::new(),
    }
}

/// The string under `key` of `v`.
pub(crate) fn str_of<'a>(v: &'a Json, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Json::as_str)
}

/// The items of the GraphQL connection at `pointer` in `v`.
pub(crate) fn nodes<'a>(v: &'a Json, pointer: &str) -> impl Iterator<Item = &'a Json> {
    v.pointer(&format!("{pointer}/nodes"))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
}

/// The most pages one list is read to. A list still going on is kept as
/// read so far and reported as read in part.
pub(crate) const MAX_PAGES: usize = 40;

/// The cursor of the page after `connection`, when there is one.
pub(crate) fn next_cursor(connection: &Json) -> Option<String> {
    match connection.pointer("/pageInfo/hasNextPage") {
        Some(Json::Bool(true)) => connection
            .pointer("/pageInfo/endCursor")
            .and_then(Json::as_str)
            .map(String::from),
        _ => None,
    }
}

/// Read the rest of the connection at `at` in `resp` into it: `more`
/// answers the query for the page after a cursor, holding that page at `at`
/// too. Past [`MAX_PAGES`] pages `hasNextPage` stays true.
pub(crate) fn follow(
    resp: &mut Json,
    at: &str,
    mut more: impl FnMut(&str) -> Result<Json>,
) -> Result<()> {
    for _ in 1..MAX_PAGES {
        let Some(cursor) = resp.pointer(at).and_then(next_cursor) else {
            return Ok(());
        };
        let mut page = more(&cursor)?;
        let next = page
            .pointer_mut(at)
            .map(Json::take)
            .filter(|c| c.is_object())
            .with_context(|| format!("the tracker answered no `{at}` after cursor `{cursor}`"))?;
        let Some(conn) = resp.pointer_mut(at) else {
            return Ok(());
        };
        if let (Some(nodes), Some(added)) = (
            conn.get_mut("nodes").and_then(Json::as_array_mut),
            next.get("nodes").and_then(Json::as_array),
        ) {
            nodes.extend(added.iter().cloned());
        }
        conn["pageInfo"] = next["pageInfo"].clone();
    }
    Ok(())
}

/// The connection at `at` read to its end, or [`MAX_PAGES`] of it: `page`
/// answers the query for the page after a cursor, or for the first page
/// given `None`, holding that page at `at`. Null when the first answer has
/// nothing there.
pub(crate) fn read_all(
    at: &str,
    mut page: impl FnMut(Option<&str>) -> Result<Json>,
) -> Result<Json> {
    let mut resp = page(None)?;
    follow(&mut resp, at, |cursor| page(Some(cursor)))?;
    Ok(resp.pointer_mut(at).map(Json::take).unwrap_or(Json::Null))
}

/// For [`Discovered::truncated`]: a sentence saying the list of `what` at
/// `conn` was read only in part, when it has pages left unread.
pub(crate) fn partly(conn: Option<&Json>, what: &str) -> Option<String> {
    let conn = conn.filter(|c| unfinished(Some(c)))?;
    Some(format!(
        "only the first {} {what} were read",
        nodes(conn, "").count()
    ))
}

/// Whether the connection `conn` has pages left unread.
pub(crate) fn unfinished(conn: Option<&Json>) -> bool {
    conn.and_then(|c| c.pointer("/pageInfo/hasNextPage")) == Some(&Json::Bool(true))
}

/// An enum value read from a node's `id` and `name`, valid in every team.
pub(crate) fn id_name(v: &Json) -> Option<EnumValue> {
    Some(EnumValue {
        name: str_of(v, "name")?.to_string(),
        id: str_of(v, "id")?.to_string(),
        teams: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_list_is_read_page_by_page_to_its_end() {
        let pages = [
            json!({ "data": { "l": { "pageInfo": { "hasNextPage": true, "endCursor": "c1" }, "nodes": [1, 2] } } }),
            json!({ "data": { "l": { "pageInfo": { "hasNextPage": true, "endCursor": "c2" }, "nodes": [3] } } }),
            json!({ "data": { "l": { "pageInfo": { "hasNextPage": false }, "nodes": [4] } } }),
        ];
        let mut asked = Vec::new();
        let read = read_all("/data/l", |after| {
            asked.push(after.map(String::from));
            Ok(pages[asked.len() - 1].clone())
        })
        .unwrap();
        assert_eq!(asked, [None, Some("c1".into()), Some("c2".into())]);
        assert_eq!(read["nodes"], json!([1, 2, 3, 4]));
        assert_eq!(partly(Some(&read), "items"), None);
    }

    #[test]
    fn a_list_longer_than_the_page_cap_is_reported_in_part() {
        let mut n = 0;
        let read = read_all("/l", |_| {
            n += 1;
            Ok(json!({ "l": { "pageInfo": { "hasNextPage": true, "endCursor": format!("c{n}") }, "nodes": [n] } }))
        })
        .unwrap();
        assert_eq!(n, MAX_PAGES);
        assert_eq!(
            partly(Some(&read), "labels").as_deref(),
            Some(format!("only the first {MAX_PAGES} labels were read").as_str())
        );
    }

    #[test]
    fn field_names_are_lowercase_with_underscores() {
        assert_eq!(field_name("Priority"), "priority");
        assert_eq!(field_name(" Story points (est.) "), "story_points_est");
    }
}
