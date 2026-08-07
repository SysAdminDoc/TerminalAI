//! Does the agent actually accept the arguments this tool builds?
//!
//! The launch goldens (`tests/fixtures/launch/*.json`) pin the argument vector
//! for a named CLI version, and they are worth having precisely because their
//! authority comes from the vendor's `--help` rather than from this code. But
//! they assert what this tool *emits*, and an agent's answer to an argv is a
//! separate fact: a flag that was removed upstream, or one added in a later
//! release than the installed binary, produces a golden that passes and a
//! launch that is refused before the agent starts. That gap is real — two
//! roadmap items were blocked in v0.19.0 for exactly it — and nothing in the
//! test suite could see it, because no test runs the agent.
//!
//! This is the pure half: given a help text and an argv, which flags does the
//! help not list. Reading the help out of a real process belongs to the probe,
//! which is where machine-facing behaviour is verified in this workspace.
//!
//! # Telling a flag from a value that looks like one
//!
//! An argv is a flat list, so "starts with `--`" is not enough. This is not
//! hypothetical: both launch goldens pass `--dangerously-skip-permissions` as
//! the *initial prompt*, deliberately, to prove the prompt is not re-read as a
//! flag. Reporting it here would be a false absence on the one case those
//! fixtures exist to pin.
//!
//! The answer is structural rather than a guess. `--` is the POSIX
//! end-of-options marker and this tool already emits it before the prompt, so
//! scanning stops there and everything after it is positional by definition.
//! Any remaining case — a value that looks like a flag *before* the marker —
//! is declared explicitly by the caller, because a wrong guess about that
//! would be silent.

/// A token that is syntactically a flag: begins with `-` and is more than a
/// lone dash or the `--` end-of-options marker.
fn looks_like_flag(token: &str) -> bool {
    token.starts_with('-') && token != "-" && token != "--"
}

/// The flag part of a token, dropping any `=value` suffix.
fn flag_name(token: &str) -> &str {
    match token.split_once('=') {
        Some((name, _)) => name,
        None => token,
    }
}

/// Every distinct flag an argv uses, in first-seen order, excluding the tokens
/// the caller has declared to be values.
///
/// Order is preserved rather than sorted so a report reads in the order the
/// launcher built the command, which is how a reader will look for it.
pub fn flags_used<'a>(args: &'a [String], values: &[String]) -> Vec<&'a str> {
    let mut seen: Vec<&str> = Vec::new();
    for token in args {
        // Everything after the end-of-options marker is positional. This is
        // what keeps the initial prompt out of the report without anyone having
        // to remember to declare it.
        if token == "--" {
            break;
        }
        // Declared values are skipped by identity. Deliberately not "skip the
        // token after a flag" — that rule cannot tell `--verbose --model opus`
        // (two flags) from `--model opus` (one), and would silently drop every
        // flag that follows another. Declaring the values is the whole reason
        // this function takes them.
        if values.iter().any(|value| value == token) {
            continue;
        }
        if !looks_like_flag(token) {
            continue;
        }
        let name = flag_name(token);
        if !seen.contains(&name) {
            seen.push(name);
        }
    }
    seen
}

/// Does this help text list `flag` as an option?
///
/// Matched on a whole-token boundary, because a substring test says `--model`
/// is present in a help that only documents `--models`, and reports a flag as
/// accepted that the CLI would reject. The character after the match must not
/// continue the name.
pub fn help_lists_flag(help: &str, flag: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = help[from..].find(flag) {
        let start = from + offset;
        let end = start + flag.len();
        let before_ok = start == 0
            || !help[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '-');
        let after_ok = !help[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '-');
        if before_ok && after_ok {
            return true;
        }
        // Advance by one character, not one byte: `help` is arbitrary text and
        // slicing mid-codepoint would panic.
        from = start + help[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// The flags in `args` that `help` does not list.
///
/// An empty result is the only good answer. A non-empty one names arguments
/// this tool would build and the installed agent would refuse.
pub fn unlisted_flags<'a>(help: &str, args: &'a [String], values: &[String]) -> Vec<&'a str> {
    flags_used(args, values)
        .into_iter()
        .filter(|flag| !help_lists_flag(help, flag))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_value_that_looks_like_a_flag_is_not_reported_as_one() {
        // The canonical golden really does pass this string as the initial
        // prompt. Reporting it would be a false absence on the exact case the
        // fixture exists to pin.
        // The declared-value path, for a flag-shaped value that is NOT behind
        // the end-of-options marker. The marker handles the goldens; this
        // handles anything that is not so tidy.
        let args = owned(&["--model", "opus", "--dangerously-skip-permissions"]);
        let values = owned(&["--dangerously-skip-permissions"]);
        assert_eq!(flags_used(&args, &values), vec!["--model"]);
        assert!(unlisted_flags("  --model <name>\n", &args, &values).is_empty());
    }

    #[test]
    fn a_prefix_of_a_documented_flag_is_not_accepted() {
        // The whole reason for the boundary check: a substring test passes here
        // and the CLI would reject the launch.
        assert!(!help_lists_flag("  --models   list available models\n", "--model"));
        assert!(help_lists_flag("  --model <name>\n", "--model"));
    }

    #[test]
    fn a_flag_joined_to_its_value_is_matched_by_name() {
        let args = owned(&["--effort=high"]);
        assert_eq!(flags_used(&args, &[]), vec!["--effort"]);
        assert!(unlisted_flags("  --effort <level>", &args, &[]).is_empty());
    }

    #[test]
    fn a_flag_the_help_does_not_list_is_reported() {
        let args = owned(&["--ax-screen-reader", "--model", "opus"]);
        let values = owned(&["opus"]);
        // Claude Code 2.1.170's help lists neither of the blocked flags; this is
        // the shape of the failure that blocked two roadmap items.
        assert_eq!(
            unlisted_flags("  --model <name>\n", &args, &values),
            vec!["--ax-screen-reader"],
        );
    }

    #[test]
    fn a_flag_following_another_flag_is_still_seen() {
        // The first version of this skipped the token after every flag, so a
        // boolean flag swallowed the one after it and the report went quiet
        // about arguments the agent would refuse. Two of the tests above passed
        // anyway, because the swallowed flag happened to be a documented one.
        let args = owned(&["--verbose", "--model", "opus", "--ax-screen-reader"]);
        let values = owned(&["opus"]);
        assert_eq!(
            flags_used(&args, &values),
            vec!["--verbose", "--model", "--ax-screen-reader"],
        );
        assert_eq!(
            unlisted_flags("  --verbose\n  --model <name>\n", &args, &values),
            vec!["--ax-screen-reader"],
        );
    }

    #[test]
    fn scanning_stops_at_the_end_of_options_marker() {
        // Exactly the shape both launch goldens end with. Without the break,
        // the initial prompt is reported as a flag the agent does not accept —
        // a false failure on the case the fixtures exist to pin.
        let args = owned(&["--verbose", "--", "--dangerously-skip-permissions"]);
        assert_eq!(flags_used(&args, &[]), vec!["--verbose"]);
        assert!(unlisted_flags("  --verbose\n", &args, &[]).is_empty());
    }

    #[test]
    fn a_bare_dash_is_not_a_flag() {
        let args = owned(&["-", "-p"]);
        assert_eq!(flags_used(&args, &[]), vec!["-p"]);
    }

    #[test]
    fn a_flag_at_the_very_start_of_the_help_is_found() {
        // `start == 0` has no preceding character to inspect, and getting that
        // boundary wrong reports a documented flag as missing.
        assert!(help_lists_flag("--verbose  say more", "--verbose"));
    }

    #[test]
    fn a_help_containing_multibyte_text_does_not_panic() {
        // The scan advances by character rather than byte; a byte step would
        // slice mid-codepoint and panic on any non-ASCII help text.
        let help = "  --modèle <nom>\n  --model <name>\n";
        assert!(help_lists_flag(help, "--model"));
        assert!(!help_lists_flag("  — an em dash —", "--model"));
    }
}
