//! Rust-owned terminal grids for sessions that are not currently focused.
//!
//! The browser owns exactly one xterm renderer. Every session still needs a
//! terminal state because an agent can redraw, switch to an alternate screen,
//! or move its cursor while it is in the background. This module consumes the
//! same PTY bytes as the scrollback ring and keeps that state compactly in
//! Rust with `vte`.

use std::mem;

use unicode_width::UnicodeWidthChar;
use vte::ansi::{
    ClearMode, Handler, LineClearMode, Mode, NamedMode, NamedPrivateMode, PrivateMode, Processor,
    TabulationClearMode,
};

pub const DEFAULT_GRID_ROWS: u16 = 40;
pub const DEFAULT_GRID_COLS: u16 = 120;
const DEFAULT_TAB_INTERVAL: usize = 8;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    character: char,
    continuation: bool,
}

impl Cell {
    const fn blank() -> Self {
        Self {
            character: ' ',
            continuation: false,
        }
    }
}

#[derive(Debug, Clone)]
struct Screen {
    rows: usize,
    cols: usize,
    cells: Vec<Cell>,
    cursor_row: usize,
    cursor_col: usize,
    wrap_pending: bool,
    auto_wrap: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    tabstops: Vec<bool>,
}

impl Screen {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            cells: vec![Cell::blank(); rows * cols],
            cursor_row: 0,
            cursor_col: 0,
            wrap_pending: false,
            auto_wrap: true,
            scroll_top: 0,
            scroll_bottom: rows,
            tabstops: default_tabstops(cols),
        }
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let mut cells = vec![Cell::blank(); rows * cols];
        let copy_rows = self.rows.min(rows);
        let copy_cols = self.cols.min(cols);
        for row in 0..copy_rows {
            let old_start = row * self.cols;
            let new_start = row * cols;
            cells[new_start..new_start + copy_cols]
                .copy_from_slice(&self.cells[old_start..old_start + copy_cols]);
        }
        self.rows = rows;
        self.cols = cols;
        self.cells = cells;
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.cursor_col = self.cursor_col.min(cols - 1);
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows;
        self.tabstops = default_tabstops(cols);
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let top = top.saturating_sub(1).min(self.rows - 1);
        let bottom = bottom.unwrap_or(self.rows).min(self.rows);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.rows;
        }
        self.wrap_pending = false;
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn cell_index(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    fn put(&mut self, c: char, width: usize) {
        if self.wrap_pending {
            self.linefeed();
            self.carriage_return();
        }
        let width = width.clamp(1, 2);
        if self.cursor_col + width > self.cols && self.auto_wrap {
            self.linefeed();
            self.carriage_return();
        }
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        let width = width.min(self.cols - self.cursor_col).max(1);
        self.clear_wide_at(self.cursor_row, self.cursor_col);
        let index = self.cell_index(self.cursor_row, self.cursor_col);
        self.cells[index] = Cell {
            character: c,
            continuation: false,
        };
        if width == 2 {
            self.clear_wide_at(self.cursor_row, self.cursor_col + 1);
            self.cells[index + 1] = Cell {
                character: ' ',
                continuation: true,
            };
        }
        if self.cursor_col + width >= self.cols {
            self.cursor_col = self.cols - 1;
            self.wrap_pending = self.auto_wrap;
        } else {
            self.cursor_col += width;
            self.wrap_pending = false;
        }
    }

    fn clear_wide_at(&mut self, row: usize, col: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let index = self.cell_index(row, col);
        if self.cells[index].continuation {
            self.cells[index] = Cell::blank();
            if col > 0 {
                self.cells[index - 1] = Cell::blank();
            }
            return;
        }
        self.cells[index] = Cell::blank();
        if col + 1 < self.cols && self.cells[index + 1].continuation {
            self.cells[index + 1] = Cell::blank();
        }
    }

    fn scroll_up_all(&mut self, count: usize) {
        self.scroll_region_up(0, self.rows, count);
    }

    fn scroll_down_all(&mut self, count: usize) {
        self.scroll_region_down(0, self.rows, count);
    }

    fn scroll_region_up(&mut self, top: usize, bottom: usize, count: usize) {
        let count = count.min(bottom.saturating_sub(top));
        let shift = count * self.cols;
        let start = top * self.cols;
        let end = bottom * self.cols;
        if shift == 0 || start >= end {
            return;
        }
        self.cells.copy_within(start + shift..end, start);
        self.cells[end - shift..end].fill(Cell::blank());
    }

    fn scroll_region_down(&mut self, top: usize, bottom: usize, count: usize) {
        let count = count.min(bottom.saturating_sub(top));
        let shift = count * self.cols;
        let start = top * self.cols;
        let end = bottom * self.cols;
        if shift == 0 || start >= end {
            return;
        }
        self.cells.copy_within(start..end - shift, start + shift);
        self.cells[start..start + shift].fill(Cell::blank());
    }

    fn in_scroll_region(&self) -> bool {
        self.cursor_row >= self.scroll_top && self.cursor_row < self.scroll_bottom
    }

    fn next_tabstop(&self) -> usize {
        self.tabstops
            .iter()
            .enumerate()
            .skip(self.cursor_col.saturating_add(1))
            .find_map(|(col, set)| set.then_some(col))
            .unwrap_or(self.cols - 1)
    }

    fn set_tabstop(&mut self) {
        self.tabstops[self.cursor_col.min(self.cols - 1)] = true;
    }

    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        match mode {
            TabulationClearMode::Current => {
                self.tabstops[self.cursor_col.min(self.cols - 1)] = false;
            }
            TabulationClearMode::All => self.tabstops.fill(false),
        }
    }

    fn set_tabs(&mut self, interval: u16) {
        let interval = usize::from(interval).max(1);
        self.tabstops.fill(false);
        for col in (0..self.cols).step_by(interval) {
            self.tabstops[col] = true;
        }
    }

    fn snapshot(&self) -> TerminalGridSnapshot {
        let lines = self
            .cells
            .chunks(self.cols)
            .map(|line| {
                line.iter()
                    .filter(|cell| !cell.continuation)
                    .map(|cell| cell.character)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
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

fn default_tabstops(cols: usize) -> Vec<bool> {
    (0..cols)
        .map(|col| col % DEFAULT_TAB_INTERVAL == 0)
        .collect()
}

impl Screen {
    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.in_scroll_region() && self.cursor_row + 1 >= self.scroll_bottom {
            self.scroll_region_up(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll_up_all(1);
        }
    }

    fn carriage_return(&mut self) {
        self.wrap_pending = false;
        self.cursor_col = 0;
    }

    fn scroll_up(&mut self, count: usize) {
        self.scroll_region_up(self.scroll_top, self.scroll_bottom, count);
    }

    fn scroll_down(&mut self, count: usize) {
        self.scroll_region_down(self.scroll_top, self.scroll_bottom, count);
    }

    fn insert_blank(&mut self, count: usize) {
        self.wrap_pending = false;
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        let count = count.min(self.cols - self.cursor_col);
        let row_start = self.cursor_row * self.cols;
        let start = row_start + self.cursor_col;
        let end = row_start + self.cols;
        if count == 0 {
            return;
        }
        self.cells.copy_within(start..end - count, start + count);
        self.cells[start..start + count].fill(Cell::blank());
    }

    fn delete_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        let count = count.min(self.cols - self.cursor_col);
        let row_start = self.cursor_row * self.cols;
        let start = row_start + self.cursor_col;
        let end = row_start + self.cols;
        if count == 0 {
            return;
        }
        self.cells.copy_within(start + count..end, start);
        self.cells[end - count..end].fill(Cell::blank());
    }

    fn insert_lines(&mut self, count: usize) {
        if !self.in_scroll_region() {
            return;
        }
        let count = count.min(self.scroll_bottom.saturating_sub(self.cursor_row));
        let start = self.cursor_row * self.cols;
        let end = self.scroll_bottom * self.cols;
        let shift = count * self.cols;
        if shift == 0 {
            return;
        }
        if start + shift < end {
            self.cells.copy_within(start..end - shift, start + shift);
        }
        self.cells[start..(start + shift).min(end)].fill(Cell::blank());
    }

    fn delete_lines(&mut self, count: usize) {
        if !self.in_scroll_region() {
            return;
        }
        let count = count.min(self.scroll_bottom.saturating_sub(self.cursor_row));
        let start = self.cursor_row * self.cols;
        let end = self.scroll_bottom * self.cols;
        let shift = count * self.cols;
        if shift == 0 {
            return;
        }
        if start + shift < end {
            self.cells.copy_within(start + shift..end, start);
        }
        self.cells[end - shift..end].fill(Cell::blank());
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        self.wrap_pending = false;
        let start = self.cursor_row * self.cols;
        match mode {
            LineClearMode::Right => self.cells
                [start + self.cursor_col.min(self.cols)..start + self.cols]
                .fill(Cell::blank()),
            LineClearMode::Left => self.cells
                [start..=start + self.cursor_col.min(self.cols.saturating_sub(1))]
                .fill(Cell::blank()),
            LineClearMode::All => self.cells[start..start + self.cols].fill(Cell::blank()),
        }
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        self.wrap_pending = false;
        let cursor = self.cursor_row * self.cols + self.cursor_col.min(self.cols);
        match mode {
            ClearMode::Below => {
                if self.cursor_col < self.cols {
                    self.cells[cursor..(self.cursor_row + 1) * self.cols].fill(Cell::blank());
                }
                self.cells[(self.cursor_row + 1) * self.cols..].fill(Cell::blank());
            }
            ClearMode::Above => {
                let cursor = cursor.min(self.cells.len());
                self.cells[..cursor].fill(Cell::blank());
            }
            ClearMode::All => self.cells.fill(Cell::blank()),
            // There is no scrollback in Screen; the bounded byte ring owns
            // replay history, so CSI 3 J must not erase visible cells.
            ClearMode::Saved => {}
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

    fn resize(&mut self, rows: usize, cols: usize) {
        self.screen.resize(rows, cols);
        if let Some(saved) = self.saved_main_screen.as_mut() {
            saved.resize(rows, cols);
        }
        self.saved_cursor.0 = self.saved_cursor.0.min(self.screen.rows - 1);
        self.saved_cursor.1 = self.saved_cursor.1.min(self.screen.cols - 1);
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
        let width = UnicodeWidthChar::width(c).unwrap_or(1).max(1);
        if self.insert_mode {
            self.screen.insert_blank(width);
        }
        self.screen.put(c, width);
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
            self.screen.cursor_col = self.screen.next_tabstop();
        }
    }

    fn set_horizontal_tabstop(&mut self) {
        self.screen.set_tabstop();
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
        self.screen.cells[start..end].fill(Cell::blank());
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
        if self.screen.in_scroll_region() && self.screen.cursor_row == self.screen.scroll_top {
            self.screen.scroll_down(1);
        } else if self.screen.cursor_row == 0 {
            self.screen.scroll_down_all(1);
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
        match mode {
            PrivateMode::Named(NamedPrivateMode::LineWrap) => self.screen.auto_wrap = true,
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor)
            | PrivateMode::Unknown(47 | 1047 | 1049) => self.enter_alternate_screen(),
            _ => {}
        }
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        match mode {
            PrivateMode::Named(NamedPrivateMode::LineWrap) => self.screen.auto_wrap = false,
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor)
            | PrivateMode::Unknown(47 | 1047 | 1049) => self.leave_alternate_screen(),
            _ => {}
        }
    }

    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        self.screen.clear_tabs(mode);
    }

    fn set_tabs(&mut self, interval: u16) {
        self.screen.set_tabs(interval);
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        self.screen.set_scrolling_region(top, bottom);
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

    /// Resize the parsed screen while retaining the portion that still fits.
    /// The parser itself is independent of terminal dimensions and therefore
    /// continues across the resize without losing an incomplete escape.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.state
            .resize(rows.max(1) as usize, cols.max(1) as usize);
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
    use proptest::prelude::*;

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

    #[test]
    fn scrolling_region_keeps_header_and_footer_fixed() {
        let mut grid = TerminalGrid::new(4, 12);
        grid.advance(b"header\x1b[2;1Hrow-one\x1b[3;1Hrow-two\x1b[4;1Hfooter");
        grid.advance(b"\x1b[2;3r\x1b[2;1H\x1b[1S");
        assert_eq!(
            grid.snapshot().lines,
            vec!["header", "row-two", "", "footer"]
        );

        grid.advance(b"\x1b[3;1H\nnew-row");
        assert_eq!(
            grid.snapshot().lines,
            vec!["header", "", "new-row", "footer"]
        );
    }

    #[test]
    fn clear_saved_lines_does_not_clear_the_visible_screen() {
        let mut grid = TerminalGrid::new(2, 12);
        grid.advance(b"visible\x1b[3J");
        assert_eq!(grid.snapshot().lines, vec!["visible", ""]);
    }

    #[test]
    fn resize_retains_the_top_left_region_and_resets_margins() {
        let mut grid = TerminalGrid::new(3, 5);
        grid.advance(b"abcde\x1b[2;1Hf");
        grid.resize(2, 3);
        assert_eq!(
            grid.snapshot(),
            TerminalGridSnapshot {
                rows: 2,
                cols: 3,
                cursor_row: 1,
                cursor_col: 1,
                lines: vec!["abc".into(), "f".into()],
            }
        );
        grid.resize(4, 6);
        assert_eq!(grid.snapshot().lines, vec!["abc", "f", "", ""]);
    }

    #[test]
    fn wide_characters_consume_two_cells_and_wrap_safely() {
        let mut grid = TerminalGrid::new(2, 4);
        grid.advance("ab界x".as_bytes());
        let snapshot = grid.snapshot();
        assert_eq!(snapshot.lines, vec!["ab界", "x"]);
        assert_eq!((snapshot.cursor_row, snapshot.cursor_col), (1, 1));
    }

    #[test]
    fn custom_tab_stops_replace_the_default_eight_column_stops() {
        let mut grid = TerminalGrid::new(1, 10);
        grid.advance(b"a\tb");
        assert_eq!(grid.snapshot().lines[0], "a       b");

        grid.reset();
        grid.advance(b"\x1b[3g\x1b[5G\x1bH\x1b[1Ga\tb");
        assert_eq!(grid.snapshot().lines[0], "a   b");
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_keep_grid_state_in_bounds(
            bytes in proptest::collection::vec(any::<u8>(), 0..2048)
        ) {
            let mut grid = TerminalGrid::new(4, 9);
            grid.advance(&bytes);
            let snapshot = grid.snapshot();
            prop_assert_eq!(snapshot.lines.len(), 4);
            prop_assert!(snapshot.lines.iter().all(|line| line.chars().count() <= 9));
            prop_assert!(snapshot.cursor_row < 4);
            prop_assert!(snapshot.cursor_col < 9);
        }
    }
}
