use terminalai_core::TerminalGrid;

struct ReferenceStream {
    name: &'static str,
    bytes: &'static [u8],
    expected_lines: &'static [&'static str],
}

// Stable byte captures from the installed CLIs' non-interactive `--help`
// headers. Keeping the captures in source avoids credentials, network access,
// and version-dependent subprocesses in the parser regression suite.
const CLAUDE_CODE_HELP: &[u8] = b"Usage: claude [options] [command] [prompt]\r\n\
Claude Code - starts an interactive session by default, use -p/--print for\r\n\
non-interactive output\r\n";
const CODEX_HELP: &[u8] = b"Codex CLI\r\n\r\n\
If no subcommand is specified, options will be forwarded to the interactive CLI.\r\n";

#[test]
fn recorded_agent_streams_match_reference_grids_across_parser_chunks() {
    let references = [
        ReferenceStream {
            name: "claude-code-help",
            bytes: CLAUDE_CODE_HELP,
            expected_lines: &[
                "Usage: claude [options] [command] [prompt]",
                "Claude Code - starts an interactive session by default, use -p/--print for",
                "non-interactive output",
                "",
            ],
        },
        ReferenceStream {
            name: "codex-help",
            bytes: CODEX_HELP,
            expected_lines: &[
                "Codex CLI",
                "",
                "If no subcommand is specified, options will be forwarded to the interactive CLI.",
                "",
            ],
        },
    ];

    for reference in references {
        let mut grid = TerminalGrid::new(4, 120);
        for chunk in reference.bytes.chunks(7) {
            grid.advance(chunk);
        }
        let snapshot = grid.snapshot();
        assert_eq!(
            snapshot.lines, reference.expected_lines,
            "{}",
            reference.name
        );
        assert!(snapshot.cursor_row < snapshot.rows, "{}", reference.name);
        assert!(snapshot.cursor_col < snapshot.cols, "{}", reference.name);
    }
}
