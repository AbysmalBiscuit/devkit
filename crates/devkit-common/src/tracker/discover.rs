//! What `ticket fields discover` read from a tracker: each place a ticket
//! field can be written, with its type and values, under the field name it
//! is added as.

use std::collections::BTreeMap;

use devkit_config::{EnumValue, FieldType, Place};

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
}

/// One place on the tracker and what it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredField {
    pub r#type: FieldType,
    pub list: bool,
    pub r#where: Place,
    /// For an `enum`, every value the tracker offers, in its order.
    pub values: Vec<EnumValue>,
}

impl DiscoveredField {
    pub fn new(r#type: FieldType, r#where: Place) -> Self {
        DiscoveredField {
            r#type,
            list: false,
            r#where,
            values: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_are_lowercase_with_underscores() {
        assert_eq!(field_name("Priority"), "priority");
        assert_eq!(field_name(" Story points (est.) "), "story_points_est");
    }
}
