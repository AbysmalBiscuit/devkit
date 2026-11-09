//! Names devkit renamed, kept so a caller using an old name still reaches the
//! item. Each namespace (templates, MCP actions, doctor rows) holds its own
//! table, and every table goes through the lookups here.

/// One item's old name and the name that replaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Renamed {
    pub old: &'static str,
    pub new: &'static str,
}

/// The current name for `name`: the new name when `name` is an old one in
/// `table`, else `name` itself.
pub fn current<'a>(table: &[Renamed], name: &'a str) -> &'a str {
    table
        .iter()
        .find_map(|r| (r.old == name).then_some(r.new))
        .unwrap_or(name)
}

/// The old name of the current name `name`, when `table` renamed it.
pub fn old(table: &[Renamed], name: &str) -> Option<&'static str> {
    table.iter().find_map(|r| (r.new == name).then_some(r.old))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: [Renamed; 1] = [Renamed {
        old: "issue.prs",
        new: "ticket.prs",
    }];

    #[test]
    fn an_old_name_resolves_to_its_new_one_and_any_other_name_to_itself() {
        assert_eq!(current(&TABLE, "issue.prs"), "ticket.prs");
        assert_eq!(current(&TABLE, "ticket.prs"), "ticket.prs");
        assert_eq!(current(&TABLE, "ticket.status"), "ticket.status");
    }

    #[test]
    fn a_new_name_gives_back_its_old_one() {
        assert_eq!(old(&TABLE, "ticket.prs"), Some("issue.prs"));
        assert_eq!(old(&TABLE, "issue.prs"), None);
    }
}
