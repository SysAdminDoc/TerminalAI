//! Who else is inside one supervised session.
//!
//! Since agent teams, a single row can be a lead plus several *separate* Claude
//! Code instances started by it. The fleet shows that as one row with one
//! status, one cost and one memory figure — which is defensible only if the
//! operator can see what the row actually holds. Density is the whole argument
//! for this tool, and density stops being a virtue the moment a row hides work.
//!
//! # Names, not a count
//!
//! The number of processes a row is comes from the job object
//! (`process_tree::JobUsage`), which measures rather than reads a file, and it
//! is already on the row. This module answers the different question the job
//! cannot: *which* teammates, by the names the operator gave them.
//!
//! # Derived from the id this tool assigned
//!
//! The team directory is named after the session id, and there are two ids in
//! play. `LaunchSpec::session_id` is assigned here, at launch, and passed to the
//! agent as `--session-id`; `Session::resume_id` is populated later from an
//! ingested hook and is absent until the session reports one. Only the first is
//! available when a row starts, so only the first is used.
//!
//! # Absence is never a zero
//!
//! Agent teams is opt-in and experimental: for almost every session this file
//! does not exist, and that is the ordinary case rather than an error. A missing
//! directory, an unreadable file, a shape this does not recognise and an empty
//! member list all produce `None` — nothing is rendered — because "no team" and
//! "a team of nobody" would read identically on a row and only one of them is
//! ever true.

use std::path::{Path, PathBuf};

/// Where Claude Code keeps one directory per live team.
const TEAMS_DIR: &str = ".claude/teams";

/// How much of the session id names the directory.
const NAME_PREFIX_LEN: usize = 8;

/// Longest member list that will be read back. A row has space for a handful of
/// names; past that the useful thing to say is the count, which the job's own
/// process figure already gives.
pub const MAX_MEMBERS: usize = 16;

#[derive(Debug, serde::Deserialize)]
struct RawTeam {
    #[serde(default)]
    members: Vec<RawMember>,
}

#[derive(Debug, serde::Deserialize)]
struct RawMember {
    #[serde(default)]
    name: Option<String>,
}

/// The directory a session's team would use, if it has one.
///
/// `None` for an id too short to name one, or one carrying anything that is not
/// safe as a single path component — this builds a path from a value that
/// arrives as text, and a `..` in it would walk out of the teams directory.
pub fn team_directory_name(session_id: &str) -> Option<String> {
    let head: String = session_id.chars().take(NAME_PREFIX_LEN).collect();
    if head.chars().count() < NAME_PREFIX_LEN {
        return None;
    }
    if !head
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(format!("session-{head}"))
}

/// The path this session's team configuration would live at.
pub fn team_config_path(home: &Path, session_id: &str) -> Option<PathBuf> {
    let name = team_directory_name(session_id)?;
    Some(home.join(TEAMS_DIR).join(name).join("config.json"))
}

/// The teammates a session's team names, if it has a team at all.
///
/// `home` is injected rather than read from the environment so this is testable
/// against a directory a test wrote, which is the only honest way to exercise a
/// reader of somebody else's file format.
pub fn teammates(home: &Path, session_id: &str) -> Option<Vec<String>> {
    let path = team_config_path(home, session_id)?;
    let text = std::fs::read_to_string(path).ok()?;
    parse_members(&text)
}

/// The member names in a team configuration.
///
/// Split from the read so the shape can be asserted without a filesystem, and
/// so an unfamiliar shape produces `None` at exactly one place.
pub fn parse_members(text: &str) -> Option<Vec<String>> {
    let raw: RawTeam = serde_json::from_str(text).ok()?;
    let names: Vec<String> = raw
        .members
        .into_iter()
        .filter_map(|member| member.name)
        .map(|name| name.trim().to_owned())
        // A blank name is worse than no name: it renders as a gap the operator
        // reads as a rendering fault.
        .filter(|name| !name.is_empty())
        .take(MAX_MEMBERS)
        .collect();
    (!names.is_empty()).then_some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "terminalai-teams-{}-{name}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Scratch(dir)
    }

    fn write_team(home: &Path, session_id: &str, body: &str) {
        let path = team_config_path(home, session_id).expect("a nameable id");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, body).expect("write");
    }

    const SESSION: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

    #[test]
    fn the_directory_is_named_after_the_first_eight_characters_of_the_id() {
        assert_eq!(
            team_directory_name(SESSION).as_deref(),
            Some("session-aaaaaaaa")
        );
    }

    #[test]
    fn an_id_that_could_walk_out_of_the_teams_directory_names_nothing() {
        // This builds a path from text. A separator or a parent reference in it
        // would read a file somewhere else entirely and attribute whatever it
        // found to this row.
        for hostile in ["../../etc", "..\\..\\win", "a/b/c/d/e", "short"] {
            assert_eq!(team_directory_name(hostile), None, "{hostile}");
        }
    }

    #[test]
    fn a_session_with_no_team_reports_nothing_rather_than_none_of_them() {
        // The ordinary case: agent teams is opt-in, so almost every session has
        // no such file. Reporting "0 teammates" would put a claim on every row
        // in the fleet.
        let home = scratch("no-team");
        assert_eq!(teammates(&home.0, SESSION), None);
    }

    #[test]
    fn the_members_a_team_names_are_read() {
        let home = scratch("named");
        write_team(
            &home.0,
            SESSION,
            r#"{"members":[{"name":"reviewer","agentId":"a1"},{"name":"tester","agentId":"a2"}]}"#,
        );
        assert_eq!(
            teammates(&home.0, SESSION),
            Some(vec!["reviewer".to_owned(), "tester".to_owned()])
        );
    }

    #[test]
    fn a_file_this_does_not_understand_reports_nothing_rather_than_guessing() {
        // Somebody else's format, and this build is not the last word on it. A
        // shape change must degrade to silence, never to a wrong row.
        let home = scratch("unfamiliar");
        for body in [
            "not json at all",
            "{}",
            r#"{"members":[]}"#,
            r#"{"members":[{"agentId":"a1"}]}"#,
            r#"{"members":[{"name":"   "}]}"#,
            r#"{"members":"two"}"#,
        ] {
            write_team(&home.0, SESSION, body);
            assert_eq!(teammates(&home.0, SESSION), None, "{body}");
        }
    }

    #[test]
    fn a_member_list_longer_than_a_row_can_show_is_bounded() {
        let members: Vec<String> = (0..40).map(|n| format!(r#"{{"name":"m{n}"}}"#)).collect();
        let body = format!(r#"{{"members":[{}]}}"#, members.join(","));
        let names = parse_members(&body).expect("names");
        assert_eq!(names.len(), MAX_MEMBERS);
        assert_eq!(names[0], "m0");
    }

    #[test]
    fn extra_fields_do_not_stop_it_being_read() {
        // The vendor adds fields; a strict decode would turn every addition into
        // a row that silently stops naming its teammates.
        let names = parse_members(
            r#"{"teamName":"session-aaaaaaaa","createdAt":1,"members":[{"name":"reviewer","model":"opus","extra":{"x":1}}]}"#,
        );
        assert_eq!(names, Some(vec!["reviewer".to_owned()]));
    }
}
