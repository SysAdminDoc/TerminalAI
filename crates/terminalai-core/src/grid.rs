//! Rust-owned terminal grids for sessions that are not currently focused.
//!
//! The browser owns exactly one xterm renderer. Every session still needs a
//! terminal state because an agent can redraw, switch to an alternate screen,
//! or move its cursor while it is in the background. This module consumes the
//! same PTY bytes as the scrollback ring and keeps that state compactly in
//! Rust with `vte`.

use std::mem;

use vte::ansi::{
    ClearMode, Handler, LineClearMode, Mode, NamedMode, NamedPrivateMode, PrivateMode, Processor,
};

pub const DEFAULT_GRID_ROWS: u16 = 40;
pub const DEFAULT_GRID_COLS: u16 = 120;

/// A serializable view of a terminal grid, useful for diagnostics and future
/// pinned-pane renderers. The focused xterm continues to receive raw replay
/// bytes so its full color and hyperlink behavior remains unchanged.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalGridSnapshot {
    pub rows: u16,
    pub cols: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct Screen {
    rows: usize,
    cols: usize,
    cells: Vec<char>,
    cursor_row: usize,
    cursor_col: usize,
    wrap_pending: bool,
}

impl Screen {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            cells: vec![' '; rows * cols],
            cursor_row: 0,
            cursor_col: 0,
            wrap_pending: false,
        }
    }

    fn cell_index(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    fn put(&mut self, c: char) {
        if self.wrap_pending {
            self.linefeed();
            self.carriage_return();
        }
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        let index = self.cell_index(self.cursor_row, self.cursor_col);
        self.cells[index] = c;
        if self.cursor_col + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.cursor_col += 1;
        }
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll_up(1);
        }
    }

    fn carriage_return(&mut self) {
        self.wrap_pending = false;
        self.cursor_col = 0;
    }

    fn scroll_up(&mut self, count: usize) {
        let count = count.min(self.rows);
        let shift = count * self.cols;
        if shift >= self.cells.len() {
            self.cells.fill(' ');
            return;
        }
        self.cells.copy_within(shift.., 0);
        let len = self.cells.len();
        self.cells[len - shift..].fill(' ');
    }

    fn scroll_down(&mut self, count: usize) {
        let count = count.min(self.rows);
        let shift = count * self.cols;
        if shift >= self.cells.len() {
            self.cells.fill(' ');
            return;
        }
        let end = self.cells.len() - shift;
        self.cells.copy_within(..end, shift);
        self.cells[..shift].fill(' ');
    }

    fn insert_blank(&mut self, count: usize) {
        self.wrap_pending = false;
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        let count = count.min(self.cols - self.cursor_col);
        let row_start = self.cursor_row * self.cols;
        let start = row_start + self.cursor_col;
        let end = row_start + self.cols;
        self.cells.copy_within(start..end - count, start + count);
        self.cells[start..start + count].fill(' ');
    }

    fn delete_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        let count = count.min(self.cols - self.cursor_col);
        let row_start = self.cursor_row * self.cols;
        let start = row_start + self.cursor_col;
        let end = row_start + self.cols;
        self.cells.copy_within(start + count..end, start);
        self.cells[end - count..end].fill(' ');
    }

    fn insert_lines(&mut self, count: usize) {
        let count = count.min(self.rows.saturating_sub(self.cursor_row));
        let start = self.cursor_row * self.cols;
        let end = self.cells.len();
        let shift = count * self.cols;
        if shift == 0 {
            return;
        }
        if start + shift < end {
            self.cells.copy_within(start..end - shift, start + shift);
        }
        self.cells[start..(start + shift).min(end)].fill(' ');
    }

    fn delete_lines(&mut self, count: usize) {
        let count = count.min(self.rows.saturating_sub(self.cursor_row));
        let start = self.cursor_row * self.cols;
        let end = self.cells.len();
        let shift = count * self.cols;
        if shift == 0 {
            return;
        }
        if start + shift < end {
            self.cells.copy_within(start + shift..end, start);
        }
        self.cells[end - shift..end].fill(' ');
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        self.wrap_pending = false;
        let start = self.cursor_row * self.cols;
        match mode {
            LineClearMode::Right => {
                self.cells[start + self.cursor_col.min(self.cols)..start + self.cols].fill(' ')
            }
            LineClearMode::Left => self.cells
                [start..=start + self.cursor_col.min(self.cols.saturating_sub(1))]
                .fill(' '),
            LineClearMode::All => self.cells[start..start + self.cols].fill(' '),
        }
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        self.wrap_pending = false;
        let cursor = self.cursor_row * self.cols + self.cursor_col.min(self.cols);
        match mode {
            ClearMode::Below => {
                if self.cursor_col < self.cols {
                    self.cells[cursor..(self.cursor_row + 1) * self.cols].fill(' ');
                }
                self.cells[(self.cursor_row + 1) * self.cols..].fill(' ');
            }
            ClearMode::Above => {
                let cursor = cursor.min(self.cells.len());
                self.cells[..cursor].fill(' ');
            }
            ClearMode::All | ClearMode::Saved => self.cells.fill(' '),
        }
    }

    fn snapshot(&self) -> TerminalGridSnapshot {
        let lines = self
            .cells
            .chunks(self.cols)
            .map(|line| line.iter().collect::<String>().trim_end().to_owned())
            .collect();
        TerminalGridSnapshot {
            rows: self.rows as u16,
            cols: self.cols as u16,
            cursor_row: self.cursor_row.min(self.rows.saturating_sub(1)) as u16,
            cursor_col: self.cursor_col.min(self.cols.saturating_sub(1)) as u16,
            lines,
        }
    }
}

struct GridState {
    screen: Screen,
    saved_main_screen: Option<Screen>,
    saved_cursor: (usize, usize),
    insert_mode: bool,
    newline_mode: bool,
}

impl GridState {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            screen: Screen::new(rows, cols),
            saved_main_screen: None,
            saved_cursor: (0, 0),
            insert_mode: false,
            newline_mode: false,
        }
    }

    fn reset(&mut self) {
        let rows = self.screen.rows;
        let cols = self.screen.cols;
        self.screen = Screen::new(rows, cols);
        self.saved_main_screen = None;
        self.saved_cursor = (0, 0);
        self.insert_mode = false;
        self.newline_mode = false;
    }

    fn enter_alternate_screen(&mut self) {
        if self.saved_main_screen.is_none() {
            let rows = self.screen.rows;
            let cols = self.screen.cols;
            self.saved_main_screen = Some(mem::replace(&mut self.screen, Screen::new(rows, cols)));
        }
    }

    fn leave_alternate_screen(&mut self) {
        if let Some(main) = self.saved_main_screen.take() {
            self.screen = main;
        }
    }
}

impl Handler for GridState {
    fn input(&mut self, c: char) {
        if self.insert_mode {
            self.screen.insert_blank(1);
        }
        self.screen.put(c);
    }

    fn goto(&mut self, line: i32, col: usize) {
        self.screen.wrap_pending = false;
        self.screen.cursor_row = line.max(0) as usize;
        self.screen.cursor_row = self
            .screen
            .cursor_row
            .min(self.screen.rows.saturating_sub(1));
        self.goto_col(col);
    }

    fn goto_line(&mut self, line: i32) {
        self.screen.wrap_pending = false;
        self.screen.cursor_row = line.max(0) as usize;
        self.screen.cursor_row = self
            .screen
            .cursor_row
            .min(self.screen.rows.saturating_sub(1));
    }

    fn goto_col(&mut self, col: usize) {
        self.screen.wrap_pending = false;
        self.screen.cursor_col = col.min(self.screen.cols.saturating_sub(1));
    }

    fn move_up(&mut self, count: usize) {
        self.screen.wrap_pending = false;
        self.screen.cursor_row = self.screen.cursor_row.saturating_sub(count);
    }

    fn move_down(&mut self, count: usize) {
        self.screen.wrap_pending = false;
        self.screen.cursor_row = self
            .screen
            .cursor_row
            .saturating_add(count)
            .min(self.screen.rows.saturating_sub(1));
    }

    fn move_forward(&mut self, count: usize) {
        self.screen.wrap_pending = false;
        self.screen.cursor_col = self
            .screen
            .cursor_col
            .saturating_add(count)
            .min(self.screen.cols.saturating_sub(1));
    }

    fn move_backward(&mut self, count: usize) {
        self.screen.wrap_pending = false;
        self.screen.cursor_col = self.screen.cursor_col.saturating_sub(count);
    }

    fn move_down_and_cr(&mut self, count: usize) {
        self.move_down(count);
        self.screen.carriage_return();
    }

    fn move_up_and_cr(&mut self, count: usize) {
        self.move_up(count);
        self.screen.carriage_return();
    }

    fn put_tab(&mut self, count: u16) {
        for _ in 0..count {
            let next = ((self.screen.cursor_col / 8) + 1) * 8;
            self.screen.cursor_col = next.min(self.screen.cols.saturating_sub(1));
        }
    }

    fn backspace(&mut self) {
        self.screen.wrap_pending = false;
        self.screen.cursor_col = self
            .screen
            .cursor_col
            .min(self.screen.cols)
            .saturating_sub(1);
    }

    fn carriage_return(&mut self) {
        self.screen.carriage_return();
    }

    fn linefeed(&mut self) {
        self.screen.linefeed();
        if self.newline_mode {
            self.screen.carriage_return();
        }
    }

    fn newline(&mut self) {
        self.screen.linefeed();
        self.screen.carriage_return();
    }

    fn scroll_up(&mut self, count: usize) {
        self.screen.scroll_up(count);
    }

    fn scroll_down(&mut self, count: usize) {
        self.screen.scroll_down(count);
    }

    fn insert_blank_lines(&mut self, count: usize) {
        self.screen.insert_lines(count);
    }

    fn delete_lines(&mut self, count: usize) {
        self.screen.delete_lines(count);
    }

    fn erase_chars(&mut self, count: usize) {
        self.screen.wrap_pending = false;
        self.screen.cursor_col = self
            .screen
            .cursor_col
            .min(self.screen.cols.saturating_sub(1));
        let start = self
            .screen
            .cell_index(self.screen.cursor_row, self.screen.cursor_col);
        let end = (start + count).min((self.screen.cursor_row + 1) * self.screen.cols);
        self.screen.cells[start..end].fill(' ');
    }

    fn delete_chars(&mut self, count: usize) {
        self.screen.delete_chars(count);
    }

    fn save_cursor_position(&mut self) {
        self.saved_cursor = (self.screen.cursor_row, self.screen.cursor_col);
    }

    fn restore_cursor_position(&mut self) {
        self.screen.wrap_pending = false;
        self.screen.cursor_row = self.saved_cursor.0.min(self.screen.rows.saturating_sub(1));
        self.screen.cursor_col = self.saved_cursor.1.min(self.screen.cols.saturating_sub(1));
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        self.screen.clear_line(mode);
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        self.screen.clear_screen(mode);
    }

    fn reset_state(&mut self) {
        self.reset();
    }

    fn reverse_index(&mut self) {
        if self.screen.cursor_row == 0 {
            self.screen.scroll_down(1);
        } else {
            self.screen.cursor_row -= 1;
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        match mode {
            Mode::Named(NamedMode::Insert) => self.insert_mode = true,
            Mode::Named(NamedMode::LineFeedNewLine) => self.newline_mode = true,
            Mode::Unknown(4) => self.insert_mode = true,
            Mode::Unknown(20) => self.newline_mode = true,
            _ => {}
        }
    }

    fn unset_mode(&mut self, mode: Mode) {
        match mode {
            Mode::Named(NamedMode::Insert) => self.insert_mode = false,
            Mode::Named(NamedMode::LineFeedNewLine) => self.newline_mode = false,
            Mode::Unknown(4) => self.insert_mode = false,
            Mode::Unknown(20) => self.newline_mode = false,
            _ => {}
        }
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        if matches!(
            mode,
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor)
        ) || matches!(mode, PrivateMode::Unknown(47 | 1047 | 1049))
        {
            self.enter_alternate_screen();
        }
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        if matches!(
            mode,
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor)
        ) || matches!(mode, PrivateMode::Unknown(47 | 1047 | 1049))
        {
            self.leave_alternate_screen();
        }
    }
}

/// A fixed-size ANSI terminal state backed by `vte`'s parser.
pub struct TerminalGrid {
    processor: Processor,
    state: GridState,
}

impl TerminalGrid {
    pub fn new(rows: u16, cols: u16) -> Self {
        let rows = rows.max(1) as usize;
        let cols = cols.max(1) as usize;
        Self {
            processor: Processor::new(),
            state: GridState::new(rows, cols),
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.state, bytes);
    }

    pub fn reset(&mut self) {
        self.processor = Processor::new();
        self.state.reset();
    }

    pub fn snapshot(&self) -> TerminalGridSnapshot {
        self.state.screen.snapshot()
    }
}

impl Default for TerminalGrid {
    fn default() -> Self {
        Self::new(DEFAULT_GRID_ROWS, DEFAULT_GRID_COLS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cursor_motion_and_screen_clear() {
        let mut grid = TerminalGrid::new(3, 8);
        grid.advance(b"one\r\ntwo\x1b[2;2H!\x1b[2K");
        let snapshot = grid.snapshot();
        assert_eq!(snapshot.lines, vec!["one", "", ""]);
        assert_eq!((snapshot.cursor_row, snapshot.cursor_col), (1, 2));
    }

    #[test]
    fn scrolls_and_keeps_background_state_bounded() {
        let mut grid = TerminalGrid::new(2, 4);
        grid.advance(b"1234\r\n5678\r\n90");
        let snapshot = grid.snapshot();
        assert_eq!(snapshot.lines, vec!["5678", "90"]);
        assert_eq!(snapshot.rows, 2);
        assert_eq!(snapshot.cols, 4);
    }

    #[test]
    fn alternate_screen_restores_the_main_buffer() {
        let mut grid = TerminalGrid::new(2, 8);
        grid.advance(b"main\x1b[?1049halt\x1b[?1049l");
        assert_eq!(grid.snapshot().lines, vec!["main", ""]);
    }

    #[test]
    fn split_utf8_and_escape_sequences_are_safe_across_chunks() {
        let mut grid = TerminalGrid::new(2, 8);
        grid.advance("café".as_bytes().get(..4).expect("split before final byte"));
        grid.advance("café".as_bytes().get(4..).expect("final byte"));
        assert_eq!(grid.snapshot().lines[0], "café");
    }
}
