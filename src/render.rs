use crate::app::{App, AppMode};
use crossterm::{
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{size, Clear, ClearType},
};
use std::io::{self, BufWriter, Write};
use std::rc::Rc;
use wed::core::{CharPos, CharRange, Document, LineColumn, Mode, Selection};

/// Padding for the status line, and for tabs and clipped multi-cell glyphs
/// within a row; sliced instead of `" ".repeat(n)`.
const PAD_SPACES: [u8; 4096] = [b' '; 4096];

fn pad_spaces(n: usize) -> &'static str {
    let n = n.min(PAD_SPACES.len());
    // SAFETY: `PAD_SPACES` is all ASCII spaces.
    unsafe { std::str::from_utf8_unchecked(&PAD_SPACES[..n]) }
}

/// Buffer reused across frames so painting a row does not allocate.
#[derive(Default)]
pub(crate) struct Scratch {
    /// Accumulates one run of same-styled glyphs before they are printed.
    run: String,
}

pub struct Renderer {
    writer: BufWriter<io::Stdout>,
    width: u16,
    height: u16,
    row_offset: usize,
    col_offset: usize,
    scratch: Scratch,
    cached_cursor_display: Option<(CharPos, u64, usize)>,
    cached_selection_ranges: Option<(Option<Selection>, u64, Rc<[CharRange]>)>,
    cached_matching_partner: Option<(CharPos, u64, Mode, Option<CharPos>)>,
}

impl Renderer {
    pub fn new() -> io::Result<Self> {
        let (width, height) = size()?;
        Ok(Self {
            writer: BufWriter::with_capacity(1 << 16, io::stdout()),
            width,
            height,
            row_offset: 0,
            col_offset: 0,
            scratch: Scratch::default(),
            cached_cursor_display: None,
            cached_selection_ranges: None,
            cached_matching_partner: None,
        })
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    pub fn text_height(&self) -> usize {
        (self.height as usize).saturating_sub(2)
    }

    pub fn render(&mut self, app: &App) -> io::Result<()> {
        queue!(self.writer, Hide)?;
        let text_height = (self.height as usize).saturating_sub(2);
        let text_width = self.width as usize;
        if text_height == 0 || text_width == 0 {
            self.writer.flush()?;
            return Ok(());
        }

        let editor = &app.editor;
        let document = &editor.document;
        let revision = editor.revision();
        let cursor = document.pos_to_line_col(editor.cursor);
        let cursor_display_column = match self.cached_cursor_display {
            Some((cached_cursor, cached_revision, cached_display))
                if cached_cursor == editor.cursor && cached_revision == revision =>
            {
                cached_display
            }
            _ => {
                let display = cursor_display_column(document, cursor);
                self.cached_cursor_display = Some((editor.cursor, revision, display));
                display
            }
        };
        if cursor.line < self.row_offset {
            self.row_offset = cursor.line;
        }
        if cursor.line >= self.row_offset + text_height {
            self.row_offset = cursor.line - text_height + 1;
        }
        if cursor_display_column < self.col_offset {
            self.col_offset = cursor_display_column;
        }
        if cursor_display_column >= self.col_offset + text_width {
            self.col_offset = cursor_display_column - text_width + 1;
        }

        let selection_ranges: Rc<[CharRange]> = match &self.cached_selection_ranges {
            Some((cached_selection, cached_revision, ranges))
                if *cached_selection == editor.selection && *cached_revision == revision =>
            {
                Rc::clone(ranges)
            }
            _ => {
                let ranges: Rc<[CharRange]> = editor
                    .selection
                    .as_ref()
                    .map(|selection| selection.ranges(document))
                    .unwrap_or_default()
                    .into();
                self.cached_selection_ranges =
                    Some((editor.selection.clone(), revision, Rc::clone(&ranges)));
                ranges
            }
        };
        let matching_partner = match self.cached_matching_partner {
            Some((cached_cursor, cached_revision, cached_mode, cached_partner))
                if cached_cursor == editor.cursor
                    && cached_revision == revision
                    && cached_mode == editor.mode =>
            {
                cached_partner
            }
            _ => {
                let partner = matching_partner_for_render(app);
                self.cached_matching_partner =
                    Some((editor.cursor, revision, editor.mode, partner));
                partner
            }
        };
        for screen_row in 0..text_height {
            let file_row = self.row_offset + screen_row;
            queue!(
                self.writer,
                MoveTo(0, screen_row as u16),
                Clear(ClearType::CurrentLine)
            )?;
            if file_row < document.line_count() {
                paint_line(
                    &mut self.writer,
                    Row {
                        document,
                        file_row,
                        col_offset: self.col_offset,
                        text_width,
                        selection_ranges: &selection_ranges[..],
                        matching_partner,
                    },
                    &mut self.scratch,
                )?;
            } else {
                queue!(
                    self.writer,
                    SetForegroundColor(Color::DarkGrey),
                    Print("~"),
                    ResetColor
                )?;
            }
        }

        self.render_status(app, cursor, cursor_display_column)?;
        self.render_message(app)?;

        let screen_row = (cursor.line - self.row_offset) as u16;
        let screen_col = (cursor_display_column - self.col_offset) as u16;
        let cursor_style = cursor_style_for(app.mode(), editor.mode);
        queue!(
            self.writer,
            cursor_style,
            MoveTo(screen_col, screen_row),
            Show
        )?;
        self.writer.flush()?;
        Ok(())
    }

    fn render_status(
        &mut self,
        app: &App,
        location: LineColumn,
        display_column: usize,
    ) -> io::Result<()> {
        let row = self.height.saturating_sub(2);
        queue!(self.writer, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        let filename = app
            .path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "[No Name]".to_string());
        let dirty = if app.is_dirty() { "[+]" } else { "" };
        let left = format!(" wed - {}{} ", filename, dirty);
        let right = format!(" Line: {}, Col: {} ", location.line + 1, display_column + 1);
        let width = self.width as usize;
        let left_len = left.chars().count();
        let right_len = right.chars().count();
        if left_len + right_len >= width {
            queue!(
                self.writer,
                SetBackgroundColor(Color::White),
                SetForegroundColor(Color::Black),
                Print(left.chars().take(width).collect::<String>()),
                ResetColor
            )?;
        } else {
            let gap = width - left_len - right_len;
            if gap <= PAD_SPACES.len() {
                queue!(
                    self.writer,
                    SetBackgroundColor(Color::White),
                    SetForegroundColor(Color::Black),
                    Print(left),
                    Print(pad_spaces(gap)),
                    Print(right),
                    ResetColor
                )?;
            } else {
                let padding = " ".repeat(gap);
                queue!(
                    self.writer,
                    SetBackgroundColor(Color::White),
                    SetForegroundColor(Color::Black),
                    Print(format!("{}{}{}", left, padding, right)),
                    ResetColor
                )?;
            }
        }
        Ok(())
    }

    fn render_message(&mut self, app: &App) -> io::Result<()> {
        let row = self.height.saturating_sub(1);
        queue!(self.writer, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        let owned;
        let (message, color) = if app.mode() == AppMode::QuitPrompt {
            ("Unsaved changes! Save first? (y/n/c)", Color::Red)
        } else if app.mode() == AppMode::CommandLine {
            owned = app.command_prompt().unwrap_or_else(|| ":".to_string());
            (&owned[..], Color::Cyan)
        } else {
            match app.editor.mode {
                Mode::Search => {
                    owned = app
                        .editor
                        .search_prompt()
                        .unwrap_or_else(|| app.editor.message.clone());
                    (&owned[..], Color::Yellow)
                }
                Mode::Insert => (&app.editor.message[..], Color::Green),
                Mode::CommandLine => (&app.editor.message[..], Color::Cyan),
                _ => (&app.editor.message[..], Color::White),
            }
        };
        let visible = safe_text(message, self.width as usize);
        queue!(
            self.writer,
            SetForegroundColor(color),
            Print(visible),
            ResetColor
        )?;
        Ok(())
    }
}

/// Everything needed to paint one row of the viewport.
pub(crate) struct Row<'a> {
    pub(crate) document: &'a Document,
    pub(crate) file_row: usize,
    pub(crate) col_offset: usize,
    pub(crate) text_width: usize,
    pub(crate) selection_ranges: &'a [CharRange],
    pub(crate) matching_partner: Option<CharPos>,
}

/// Paints one document line into the horizontal window
/// `[col_offset, col_offset + text_width)`.
///
/// Only the characters that can be seen are touched: the line is walked from its
/// start far enough to reach `col_offset`, then until the right edge, so the cost
/// depends on the viewport rather than on the length of the line.
pub(crate) fn paint_line<W: Write>(
    writer: &mut W,
    row: Row<'_>,
    scratch: &mut Scratch,
) -> io::Result<()> {
    let Row {
        document,
        file_row,
        col_offset,
        text_width,
        selection_ranges,
        matching_partner,
    } = row;
    // `Selection::ranges` yields ranges in ascending, non-overlapping order, so
    // a cursor that only ever moves forward is enough to answer containment.
    debug_assert!(
        selection_ranges
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start),
        "selection ranges must be sorted and disjoint",
    );
    let content = document.line_content_range(file_row);
    let line_start = content.start.0;
    let viewport_end = col_offset + text_width;
    scratch.run.reserve(text_width);
    let mut painter = RowPainter::new(writer, &mut scratch.run);
    let mut column = 0usize;
    let mut range_cursor = 0usize;
    for (char_index, character) in document.chars_from(CharPos(line_start)) {
        if char_index.0 >= content.end.0 {
            break;
        }
        let display_start = column;
        let cells = display_width(character, display_start);
        let display_end = display_start + cells;
        if display_end <= col_offset {
            column = display_end;
            continue;
        }
        if display_start >= viewport_end {
            break;
        }
        // A zero-cell character that has been scrolled past is skipped with the
        // rest of the line: emitting it would put a glyph in a cell it does not
        // own.
        if cells == 0 && display_start <= col_offset {
            column = display_end;
            continue;
        }
        while selection_ranges
            .get(range_cursor)
            .is_some_and(|range| range.end <= char_index)
        {
            range_cursor += 1;
        }
        let selected = selection_ranges
            .get(range_cursor)
            .is_some_and(|range| range.contains(char_index));
        let style = if selected {
            Some((Color::White, Color::Black))
        } else if matching_partner == Some(char_index) {
            Some((Color::Yellow, Color::Black))
        } else {
            None
        };
        // Clip by cell, not by character. The part of `character` inside the
        // viewport is `[visible_start, visible_end)` of the row.
        let visible_start = display_start.max(col_offset);
        let visible_end = display_end.min(viewport_end);
        let visible_cells = visible_end.saturating_sub(visible_start);
        if character == '\t' {
            // A tab is nothing but its expansion, so every visible cell is a
            // space. It never covers more than one tab stop.
            painter.push(pad_spaces(visible_cells), style)?;
        } else if visible_cells == cells {
            painter.push(safe_character(character).encode_utf8(&mut [0u8; 4]), style)?;
        } else {
            // Only part of a multi-cell glyph is on screen. Half a glyph has no
            // meaning to the terminal and drawing it would leave every later
            // column misaligned, so the visible cells become spaces.
            painter.push(pad_spaces(visible_cells), style)?;
        }
        column = display_end;
    }
    painter.finish()
}

/// Collects consecutive glyphs that share a style so each run becomes a single
/// `Print` instead of one escape sequence per character.
struct RowPainter<'a, W: Write> {
    writer: &'a mut W,
    run: &'a mut String,
    style: Option<(Color, Color)>,
}

impl<'a, W: Write> RowPainter<'a, W> {
    fn new(writer: &'a mut W, run: &'a mut String) -> Self {
        run.clear();
        Self {
            writer,
            run,
            style: None,
        }
    }

    fn push(&mut self, text: &str, style: Option<(Color, Color)>) -> io::Result<()> {
        if self.style != style {
            self.flush()?;
            if let Some((background, foreground)) = style {
                queue!(
                    self.writer,
                    SetBackgroundColor(background),
                    SetForegroundColor(foreground)
                )?;
            }
            self.style = style;
        }
        self.run.push_str(text);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.run.is_empty() {
            return Ok(());
        }
        let text = std::mem::take(self.run);
        match self.style {
            Some(_) => queue!(self.writer, Print(text), ResetColor)?,
            None => queue!(self.writer, Print(text))?,
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.flush()
    }
}

/// Exposed for the viewport benchmark in `benches/render.rs`.
///
/// Bounded deliberately: this runs on every frame whose cursor moved, and the
/// exact search runs to end of file whenever the delimiter under the cursor is
/// unmatched. On a large document that is the most expensive thing the editor
/// does, and it buys a decoration.
pub(crate) fn matching_partner_for_render(app: &App) -> Option<CharPos> {
    if app.editor.mode == Mode::Normal {
        wed::core::matching_partner_within(
            &app.editor.document,
            app.editor.cursor,
            wed::core::PARTNER_SCAN_LIMIT,
        )
    } else {
        None
    }
}

fn cursor_style_for(app_mode: AppMode, mode: Mode) -> SetCursorStyle {
    match (app_mode, mode) {
        (AppMode::QuitPrompt, _) | (AppMode::CommandLine, _) => SetCursorStyle::SteadyBar,
        (AppMode::Editing, Mode::Insert)
        | (AppMode::Editing, Mode::Search)
        | (AppMode::Editing, Mode::CommandLine) => SetCursorStyle::SteadyBar,
        _ => SetCursorStyle::SteadyBlock,
    }
}

pub(crate) fn display_width(character: char, column: usize) -> usize {
    crate::width::width(character, column)
}

/// Display column of `location.column` within its own line.
///
/// Computed straight from the rope without materialising the line, so a long
/// line costs O(cursor column) rather than O(line length).
///
/// A free function rather than a `Renderer` method: it reads no renderer state,
/// which lets the viewport benchmark in `benches/render.rs` call it without
/// standing up a terminal, and that needs a TTY.
pub(crate) fn cursor_display_column(document: &Document, location: LineColumn) -> usize {
    let start = document.line_start(location.line);
    let mut column = 0;
    for (_, character) in document.chars_from(start).take(location.column) {
        column += display_width(character, column);
    }
    column
}

#[cfg(test)]
fn display_column(line: &str, char_column: usize) -> usize {
    let mut column = 0;
    for character in line.chars().take(char_column) {
        column += display_width(character, column);
    }
    column
}

fn safe_character(character: char) -> char {
    if character.is_control() {
        '·'
    } else {
        character
    }
}

fn safe_text(text: &str, width: usize) -> String {
    text.chars().take(width).map(safe_character).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        cursor_display_column, cursor_style_for, display_column, display_width,
        matching_partner_for_render, paint_line, AppMode, Row, Scratch,
    };
    use crate::app::App;
    use crossterm::cursor::SetCursorStyle;
    use wed::core::{CharPos, CharRange, Document, LineColumn, Mode, SelectionKind};

    /// Bytes crossterm emits for the matching-partner colours.
    const YELLOW_BACKGROUND: &str = "\x1b[48;5;11m";
    const BLACK_FOREGROUND: &str = "\x1b[38;5;0m";
    const RESET: &str = "\x1b[0m";
    /// Bytes crossterm emits for the selection colours.
    const WHITE_BACKGROUND: &str = "\x1b[48;5;15m";

    fn paint(
        text: &str,
        file_row: usize,
        col_offset: usize,
        width: usize,
        ranges: &[CharRange],
        partner: Option<CharPos>,
    ) -> String {
        let document = Document::from_text(text);
        let mut scratch = Scratch::default();
        let mut sink: Vec<u8> = Vec::new();
        paint_line(
            &mut sink,
            Row {
                document: &document,
                file_row,
                col_offset,
                text_width: width,
                selection_ranges: ranges,
                matching_partner: partner,
            },
            &mut scratch,
        )
        .unwrap();
        String::from_utf8(sink).unwrap()
    }

    #[test]
    fn uses_block_cursor_for_normal_and_visual_modes() {
        assert!(matches!(
            cursor_style_for(AppMode::Editing, Mode::Normal),
            SetCursorStyle::SteadyBlock
        ));
        assert!(matches!(
            cursor_style_for(AppMode::Editing, Mode::Visual(SelectionKind::Characterwise)),
            SetCursorStyle::SteadyBlock
        ));
    }

    #[test]
    fn uses_bar_cursor_for_insert_and_prompt_modes() {
        assert!(matches!(
            cursor_style_for(AppMode::Editing, Mode::Insert),
            SetCursorStyle::SteadyBar
        ));
        assert!(matches!(
            cursor_style_for(AppMode::CommandLine, Mode::Normal),
            SetCursorStyle::SteadyBar
        ));
        assert!(matches!(
            cursor_style_for(AppMode::QuitPrompt, Mode::Normal),
            SetCursorStyle::SteadyBar
        ));
    }

    #[test]
    fn expands_tabs_to_terminal_tab_stops() {
        assert_eq!(display_width('\t', 0), 8);
        assert_eq!(display_width('\t', 3), 5);
        assert_eq!(display_column("\tx", 1), 8);
        assert_eq!(display_column("ab\tx", 3), 8);
    }

    #[test]
    fn clips_partial_tabs_to_the_viewport() {
        // Column 3..5 of the tab expands to exactly two spaces.
        assert_eq!(paint("\tx", 0, 3, 2, &[], None), "  ");
        // Once the tab is fully scrolled past, 'x' is drawn.
        assert_eq!(paint("\tx", 0, 8, 2, &[], None), "x");
        // Entirely scrolled past: nothing at all.
        assert_eq!(paint("\tx", 0, 9, 2, &[], None), "");
    }

    #[test]
    fn draws_plain_text_verbatim() {
        assert_eq!(paint("hello\nworld\n", 0, 0, 80, &[], None), "hello");
        assert_eq!(paint("hello\nworld\n", 1, 0, 80, &[], None), "world");
    }

    #[test]
    fn replaces_control_characters() {
        assert_eq!(paint("a\u{1}b\n", 0, 0, 80, &[], None), "a\u{b7}b");
    }

    #[test]
    fn draws_selection_and_matching_partner_with_colour() {
        let selected = paint(
            "abcd\n",
            0,
            0,
            80,
            &[CharRange::new(CharPos(1), CharPos(3))],
            None,
        );
        // One escape run for the two selected characters, one for the rest.
        assert_eq!(selected.matches(WHITE_BACKGROUND).count(), 1);
        assert!(selected.contains("bc"));

        let partner = paint("ab\n", 0, 0, 80, &[], Some(CharPos(1)));
        assert_eq!(
            partner,
            format!("a{}{}b{}", YELLOW_BACKGROUND, BLACK_FOREGROUND, RESET)
        );
    }

    #[test]
    fn coalesces_a_whole_row_into_one_print() {
        // No selection and no partner means no escape sequences at all.
        assert_eq!(
            paint("plain row of text", 0, 0, 80, &[], None),
            "plain row of text"
        );
    }

    #[test]
    fn skips_columns_left_of_the_viewport() {
        let text = "abcdefghij\n";
        assert_eq!(paint(text, 0, 4, 3, &[], None), "efg");
    }

    #[test]
    fn enables_matching_partner_highlight_only_in_normal_mode() {
        let mut app = App::new(None).unwrap();
        app.editor.insert_text_for_test("(x)");
        app.editor.cursor = CharPos(0);
        assert_eq!(matching_partner_for_render(&app), Some(CharPos(2)));
        app.editor.mode = Mode::Visual(SelectionKind::Characterwise);
        assert_eq!(matching_partner_for_render(&app), None);
    }

    #[test]
    fn wide_characters_occupy_two_cells() {
        assert_eq!(display_width('世', 0), 2);
        assert_eq!(display_width('a', 0), 1);
        assert_eq!(
            display_width('世', 1),
            2,
            "width does not depend on the column"
        );
        // Combining marks add nothing, so the column after one is unchanged.
        assert_eq!(display_width('\u{0301}', 4), 0);
        assert_eq!(display_column("e\u{0301}x", 3), 2);
    }

    #[test]
    fn paints_wide_characters_at_two_cells_each() {
        assert_eq!(paint("世界\n", 0, 0, 80, &[], None), "世界");
        assert_eq!(paint("a世b\n", 0, 0, 80, &[], None), "a世b");
    }

    #[test]
    fn clips_a_wide_character_at_the_right_edge_instead_of_drawing_half_of_it() {
        // 世 covers cells 0..=1. Showing only cell 1 has to produce a space,
        // because a lone half glyph would leave every later column misaligned.
        assert_eq!(paint("世x\n", 0, 1, 1, &[], None), " ");
        // Both cells visible: the glyph itself.
        assert_eq!(paint("世x\n", 0, 0, 2, &[], None), "世");
        // Entirely scrolled past.
        assert_eq!(paint("世x\n", 0, 2, 4, &[], None), "x");
    }

    #[test]
    fn clips_a_wide_character_at_the_left_edge() {
        // "世界" covers cells 0..=1 and 2..=3. A window of [1, 4) catches one
        // cell of 世 and both of 界.
        assert_eq!(paint("世界\n", 0, 1, 3, &[], None), " 界");
    }

    #[test]
    fn mixing_wide_and_narrow_characters_keeps_the_right_edge_exact() {
        // "a世b" is cells: a=0, 世=1..=2, b=3. A two-cell window from column 1
        // holds the whole of 世 and nothing of b.
        assert_eq!(paint("a世b\n", 0, 1, 2, &[], None), "世");
        assert_eq!(paint("a世b\n", 0, 2, 2, &[], None), " b");
    }

    #[test]
    fn cursor_display_column_counts_cells_not_characters() {
        let document = Document::from_text("世界ab\ncd\n");
        // After 世 (2 cells) + 界 (2 cells) + a + b the column is 6, not 4.
        assert_eq!(
            cursor_display_column(&document, LineColumn { line: 0, column: 4 }),
            6
        );
        assert_eq!(
            cursor_display_column(&document, LineColumn { line: 0, column: 1 }),
            2
        );
        assert_eq!(
            cursor_display_column(&document, LineColumn { line: 1, column: 2 }),
            2
        );
    }

    #[test]
    fn a_combining_mark_does_not_shift_the_cursor() {
        // "é" as e + combining acute is one cell, so the cursor after it sits
        // at column 1 whether the mark is there or not.
        let document = Document::from_text("e\u{0301}x\n");
        assert_eq!(
            cursor_display_column(&document, LineColumn { line: 0, column: 2 }),
            1
        );
    }
}
