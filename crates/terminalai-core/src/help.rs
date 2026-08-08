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

/// One option as `--help` presents it: the flags it declares, and the whole of
/// its description including the lines it wrapped onto.
///
/// The continuation lines are the point. A CLI states a flag's constraints in
/// prose beside it — "only works with --print" — and at the width these tools
/// print at, that phrase routinely lands on the *next* line. A check that reads
/// help as one flat string can find the flag and can find the constraint, and
/// has no way to know they belong together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpOption {
    /// Every flag spelling on the option line, long and short.
    pub flags: Vec<String>,
    /// The option line and its continuations, joined with single spaces.
    pub text: String,
}

/// Split a help text into per-option blocks.
///
/// The rule is indentation, which is what these CLIs actually use: a line whose
/// first non-space character starts a flag *and* which is no more indented than
/// the option it follows begins a new option; a more-indented line continues the
/// current one; a blank line ends it.
///
/// Deliberately not a parser for any one CLI's formatting. Anything it fails to
/// group produces a block with no constraint in it, which is the same answer the
/// check gave before this existed — the failure mode is silence, not a wrong
/// claim.
pub fn help_options(help: &str) -> Vec<HelpOption> {
    let mut options: Vec<HelpOption> = Vec::new();
    let mut current: Option<(usize, HelpOption)> = None;
    for line in help.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some((_, option)) = current.take() {
                options.push(option);
            }
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let starts_option = option_flags(trimmed).is_some();
        let continues = current
            .as_ref()
            .is_some_and(|(open_indent, _)| indent > *open_indent);
        if starts_option && !continues {
            if let Some((_, option)) = current.take() {
                options.push(option);
            }
            current = Some((
                indent,
                HelpOption {
                    flags: option_flags(trimmed).unwrap_or_default(),
                    text: trimmed.to_owned(),
                },
            ));
        } else if let Some((_, option)) = current.as_mut() {
            option.text.push(' ');
            option.text.push_str(trimmed);
        }
    }
    if let Some((_, option)) = current.take() {
        options.push(option);
    }
    options
}

/// The flags an option line declares, or `None` when the line is not one.
///
/// Stops at the first token that is not a flag, so `-e, --effort <level>  how
/// hard to think` yields `-e` and `--effort` and not the words after them.
fn option_flags(trimmed: &str) -> Option<Vec<String>> {
    if !trimmed.starts_with('-') {
        return None;
    }
    let mut flags = Vec::new();
    for token in trimmed.split_whitespace() {
        let token = token.trim_end_matches(',');
        if !looks_like_flag(token) {
            break;
        }
        flags.push(flag_name(token).to_owned());
    }
    (!flags.is_empty()).then_some(flags)
}

/// Phrases a CLI uses to say a flag binds only in some other mode.
///
/// Deliberately narrow. A loose pattern — "requires", "with" — matches ordinary
/// prose and would report flags that are perfectly usable, and a check that
/// cries wolf gets removed from the gate. These are the spellings observed in
/// the CLIs this tool launches; anything else reads as unrestricted, which is
/// the same answer as before this existed.
const RESTRICTION_PHRASES: [&str; 4] = [
    "only works with",
    "only valid with",
    "only available with",
    "can only be used with",
];

/// The flag an option says it only works alongside, if it says so.
///
/// Returns the *other* flag, so the caller can ask the question that matters: is
/// that mode one this tool's argv is actually in.
pub fn mode_requirement(option: &HelpOption) -> Option<String> {
    let lowered = option.text.to_ascii_lowercase();
    let start = RESTRICTION_PHRASES
        .iter()
        .filter_map(|phrase| lowered.find(phrase).map(|at| at + phrase.len()))
        .min()?;
    let mut tokens = lowered[start..].split_whitespace();
    let mut found = None;
    for token in tokens.by_ref() {
        let trimmed = token.trim_end_matches([')', ',', ';', '.']);
        if looks_like_flag(trimmed) {
            found = Some(trimmed.to_owned());
            break;
        }
        // Stop at the end of the clause: a flag named in the next sentence is
        // about something else.
        if token.ends_with('.') {
            break;
        }
    }
    found
}

/// Flags this argv uses whose own help text restricts them to a mode this argv
/// is not in.
///
/// The gap this closes is not hypothetical: `--max-budget-usd` was emitted into
/// an interactive command line for two releases while `claude --help` said, on
/// the wrapped continuation line beside it, "only works with --print". The flag
/// existed, so the listing check passed, and the cap it promised bound nothing.
///
/// Each result is the flag and the mode flag it needs. An empty result is the
/// only good answer.
pub fn mode_restricted_flags<'a>(
    help: &str,
    args: &'a [String],
    values: &[String],
) -> Vec<(&'a str, String)> {
    let used = flags_used(args, values);
    let options = help_options(help);
    used.iter()
        .filter_map(|flag| {
            let option = options
                .iter()
                .find(|option| option.flags.iter().any(|listed| listed == flag))?;
            let required = mode_requirement(option)?;
            // Satisfied is fine. The report is about a flag whose mode this
            // command line is not in, not about every flag that has one.
            (!used.iter().any(|other| *other == required)).then_some((*flag, required))
        })
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
    fn an_option_and_its_wrapped_description_are_read_as_one_block() {
        // The shape `claude --help` prints on 2.1.170, where the constraint
        // lands on the continuation line. A check that reads the help as one
        // flat string can see the flag and can see the constraint and has no way
        // to know they belong to each other.
        let help = concat!(
            "  --model <name>             the model to use\n",
            "  --max-budget-usd <amount>  Maximum dollar amount to spend on\n",
            "                             API calls (only works with --print)\n",
            "  --verbose                  say more\n",
        );
        let options = help_options(help);
        let budget = options
            .iter()
            .find(|option| option.flags.iter().any(|flag| flag == "--max-budget-usd"))
            .expect("the option is found");
        assert!(budget.text.contains("only works with --print"), "{budget:?}");
        assert_eq!(mode_requirement(budget).as_deref(), Some("--print"));
        // And the option after it is its own block rather than part of the
        // wrapped description above.
        let verbose = options
            .iter()
            .find(|option| option.flags.iter().any(|flag| flag == "--verbose"))
            .expect("the next option is separate");
        assert!(!verbose.text.contains("budget"), "{verbose:?}");
    }

    #[test]
    fn short_and_long_spellings_on_one_line_are_both_claimed_by_it() {
        let options = help_options("  -e, --effort <level>  how hard to think\n");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].flags, vec!["-e", "--effort"]);
    }

    #[test]
    fn a_flag_restricted_to_a_mode_this_argv_is_not_in_is_reported() {
        // The defect this exists for: the flag was listed, so the listing check
        // passed, and it was emitted into an interactive command line where the
        // CLI ignores it.
        let help = concat!(
            "  --print                    run non-interactively\n",
            "  --max-budget-usd <amount>  cap spend\n",
            "                             (only works with --print)\n",
        );
        let interactive = owned(&["--max-budget-usd", "5"]);
        assert_eq!(
            mode_restricted_flags(help, &interactive, &owned(&["5"])),
            vec![("--max-budget-usd", "--print".to_owned())],
        );
        // And satisfied is not a finding.
        let printing = owned(&["--print", "--max-budget-usd", "5"]);
        assert!(mode_restricted_flags(help, &printing, &owned(&["5"])).is_empty());
    }

    #[test]
    fn ordinary_prose_beside_a_flag_is_not_read_as_a_restriction() {
        // A loose pattern reports flags that work perfectly, and a check that
        // cries wolf gets removed from the gate.
        let help = concat!(
            "  --model <name>  the model to use with this session\n",
            "  --resume <id>   requires an existing session id\n",
        );
        let args = owned(&["--model", "opus", "--resume", "abc"]);
        let values = owned(&["opus", "abc"]);
        assert!(mode_restricted_flags(help, &args, &values).is_empty());
    }

    #[test]
    fn a_flag_the_help_does_not_describe_at_all_reports_no_restriction() {
        // Absence is the listing check's finding, not this one's. Reporting it
        // twice would make one failure look like two.
        let args = owned(&["--ax-screen-reader"]);
        assert!(mode_restricted_flags("  --model <name>\n", &args, &[]).is_empty());
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
