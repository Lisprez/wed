use crate::app::{App, AppMode};
use crossterm::{
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{size, Clear, ClearType},
};
use std::io::{self, BufWriter, Write};
use wed::core::{CharPos, CharRange, Document, LineColumn, Mode};

const TAB_STOP: usize = 8;

/// Buffers reused across frames so painting a row does not allocate.
#[derive(Default)]
struct Scratch {
    /// Holds the current row's document text.
    line: String,
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
        })
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
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
        let cursor = document.pos_to_line_col(editor.cursor);
        let cursor_display_column = self.cursor_display_column(document, cursor);
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

        let selection_ranges = editor
            .selection
            .as_ref()
            .map(|selection| selection.ranges(document))
            .unwrap_or_default();
        let matching_partner = matching_partner_for_render(app);
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
                        selection_ranges: &selection_ranges,
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

    /// Display column of `location.column` within its own line.
    fn cursor_display_column(&mut self, document: &Document, location: LineColumn) -> usize {
        self.scratch.line.clear();
        document.write_slice(
            document.line_content_range(location.line),
            &mut self.scratch.line,
        );
        display_column(&self.scratch.line, location.column)
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
            let padding = " ".repeat(width - left_len - right_len);
            queue!(
                self.writer,
                SetBackgroundColor(Color::White),
                SetForegroundColor(Color::Black),
                Print(format!("{}{}{}", left, padding, right)),
                ResetColor
            )?;
        }
        Ok(())
    }

    fn render_message(&mut self, app: &App) -> io::Result<()> {
        let row = self.height.saturating_sub(1);
        queue!(self.writer, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        let (message, color) = if app.mode() == AppMode::QuitPrompt {
            (
                "Unsaved changes! Save first? (y/n/c)".to_string(),
                Color::Red,
            )
        } else if app.mode() == AppMode::CommandLine {
            (
                app.command_prompt().unwrap_or_else(|| ":".to_string()),
                Color::Cyan,
            )
        } else {
            match app.editor.mode {
                Mode::Search => (
                    app.editor
                        .search_prompt()
                        .unwrap_or_else(|| app.editor.message.clone()),
                    Color::Yellow,
                ),
                Mode::Insert => (app.editor.message.clone(), Color::Green),
                Mode::CommandLine => (app.editor.message.clone(), Color::Cyan),
                _ => (app.editor.message.clone(), Color::White),
            }
        };
        let visible = safe_text(&message, self.width as usize);
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
struct Row<'a> {
    document: &'a Document,
    file_row: usize,
    col_offset: usize,
    text_width: usize,
    selection_ranges: &'a [CharRange],
    matching_partner: Option<CharPos>,
}

/// Paints one document line into the horizontal window
/// `[col_offset, col_offset + text_width)`.
///
/// Only the characters that can be seen are touched: the line is walked from its
/// start far enough to reach `col_offset`, then until the right edge, so the cost
/// depends on the viewport rather than on the length of the line.
fn paint_line<W: Write>(writer: &mut W, row: Row<'_>, scratch: &mut Scratch) -> io::Result<()> {
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
    scratch.line.clear();
    document.write_slice(content, &mut scratch.line);
    let viewport_end = col_offset + text_width;
    let start = viewport_start(&scratch.line, col_offset);

    let mut painter = RowPainter::new(writer, &mut scratch.run);
    let mut column = start.column;
    let mut range_cursor = 0usize;
    for (offset, character) in scratch.line[start.byte..].chars().enumerate() {
        let display_start = column;
        if display_start >= viewport_end {
            break;
        }
        let display_end = display_start + display_width(character, display_start);
        let char_index = CharPos(line_start + start.char_index + offset);
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
        if character == '\t' {
            let visible_start = display_start.max(col_offset);
            let visible_end = display_end.min(viewport_end);
            painter.push(
                &" ".repeat(visible_end.saturating_sub(visible_start)),
                style,
            )?;
        } else {
            painter.push(safe_character(character).encode_utf8(&mut [0u8; 4]), style)?;
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

/// Where the horizontal viewport begins inside a line.
struct ViewportStart {
    /// Byte offset to resume reading from.
    byte: usize,
    /// Char index of `byte` within the line.
    char_index: usize,
    /// Display column that `byte` sits at.
    column: usize,
}

/// Finds the first character whose display columns end past `col_offset`.
/// When the whole line ends at or before `col_offset` the byte offset is the
/// end of the line, so the caller draws nothing.
fn viewport_start(line: &str, col_offset: usize) -> ViewportStart {
    let mut column = 0;
    let mut char_index = 0;
    for (byte, character) in line.char_indices() {
        let end = column + display_width(character, column);
        if end > col_offset {
            return ViewportStart {
                byte,
                char_index,
                column,
            };
        }
        column = end;
        char_index += 1;
    }
    ViewportStart {
        byte: line.len(),
        char_index,
        column,
    }
}

fn matching_partner_for_render(app: &App) -> Option<CharPos> {
    if app.editor.mode == Mode::Normal {
        wed::core::matching_partner(&app.editor.document, app.editor.cursor)
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

fn display_width(character: char, column: usize) -> usize {
    if character == '\t' {
        TAB_STOP - (column % TAB_STOP)
    } else {
        1
    }
}

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
        cursor_style_for, display_column, display_width, matching_partner_for_render, paint_line,
        viewport_start, AppMode, Row, Scratch,
    };
    use crate::app::App;
    use crossterm::cursor::SetCursorStyle;
    use wed::core::{CharPos, CharRange, Document, Mode, SelectionKind};

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
    fn locates_the_viewport_start_inside_a_line() {
        let start = viewport_start("\tx", 0);
        assert_eq!((start.byte, start.char_index, start.column), (0, 0, 0));
        // The tab covers columns 0..8, so column 3 still starts on it.
        let start = viewport_start("\tx", 3);
        assert_eq!((start.byte, start.char_index, start.column), (0, 0, 0));
        let start = viewport_start("\tx", 8);
        assert_eq!((start.byte, start.char_index, start.column), (1, 1, 8));
        // Past the end of the line there is nothing left to draw.
        let start = viewport_start("\tx", 9);
        assert_eq!((start.byte, start.char_index, start.column), (2, 2, 9));
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
}
