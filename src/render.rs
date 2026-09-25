use crate::app::{App, AppMode};
use crossterm::{
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{size, Clear, ClearType},
};
use std::io::{self, BufWriter, Write};
use wed::core::Mode;

const TAB_STOP: usize = 8;

pub struct Renderer {
    writer: BufWriter<io::Stdout>,
    width: u16,
    height: u16,
    row_offset: usize,
    col_offset: usize,
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
        let cursor = editor.document.pos_to_line_col(editor.cursor);
        let cursor_chars: Vec<char> = editor.document.line_text(cursor.line).chars().collect();
        let cursor_display_column = display_column(&cursor_chars, cursor.column);
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
            .map(|selection| selection.ranges(&editor.document))
            .unwrap_or_default();
        let matching_partner = matching_partner_for_render(app);
        for screen_row in 0..text_height {
            let file_row = self.row_offset + screen_row;
            queue!(
                self.writer,
                MoveTo(0, screen_row as u16),
                Clear(ClearType::CurrentLine)
            )?;
            if file_row < editor.document.line_count() {
                let line = editor.document.line_text(file_row);
                let line_start = editor.document.line_start(file_row);
                let chars: Vec<char> = line.chars().collect();
                let positions = display_positions(&chars);
                let (start, end) = visible_range(&positions, self.col_offset, text_width);
                for index in start..end {
                    let character = chars[index];
                    let (display_start, display_end) = positions[index];
                    let visible_start = display_start.max(self.col_offset);
                    let visible_end = display_end.min(self.col_offset + text_width);
                    let rendered = if character == '\t' {
                        " ".repeat(visible_end.saturating_sub(visible_start))
                    } else {
                        safe_character(character).to_string()
                    };
                    let char_index = line_start.0 + index;
                    let selected = selection_ranges
                        .iter()
                        .any(|range| range.contains(wed::core::CharPos(char_index)));
                    let is_matching_partner =
                        matching_partner == Some(wed::core::CharPos(char_index));
                    if selected {
                        queue!(
                            self.writer,
                            SetBackgroundColor(Color::White),
                            SetForegroundColor(Color::Black),
                            Print(rendered),
                            ResetColor
                        )?;
                    } else if is_matching_partner {
                        queue!(
                            self.writer,
                            SetBackgroundColor(Color::Yellow),
                            SetForegroundColor(Color::Black),
                            Print(rendered),
                            ResetColor
                        )?;
                    } else {
                        queue!(self.writer, Print(rendered))?;
                    }
                }
            } else {
                queue!(
                    self.writer,
                    SetForegroundColor(Color::DarkGrey),
                    Print("~"),
                    ResetColor
                )?;
            }
        }

        self.render_status(app)?;
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

    fn render_status(&mut self, app: &App) -> io::Result<()> {
        let row = self.height.saturating_sub(2);
        queue!(self.writer, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        let location = app.editor.document.pos_to_line_col(app.editor.cursor);
        let line_chars: Vec<char> = app
            .editor
            .document
            .line_text(location.line)
            .chars()
            .collect();
        let display_column = display_column(&line_chars, location.column);
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

fn matching_partner_for_render(app: &App) -> Option<wed::core::CharPos> {
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

fn display_positions(chars: &[char]) -> Vec<(usize, usize)> {
    let mut column = 0;
    chars
        .iter()
        .map(|character| {
            let start = column;
            column += display_width(*character, column);
            (start, column)
        })
        .collect()
}

fn display_column(chars: &[char], char_column: usize) -> usize {
    let mut column = 0;
    for character in chars.iter().take(char_column) {
        column += display_width(*character, column);
    }
    column
}

fn visible_range(positions: &[(usize, usize)], col_offset: usize, width: usize) -> (usize, usize) {
    let viewport_end = col_offset + width;
    let start = positions
        .iter()
        .position(|(_, end)| *end > col_offset)
        .unwrap_or(positions.len());
    let end = positions
        .iter()
        .position(|(start, _)| *start >= viewport_end)
        .unwrap_or(positions.len());
    (start, end)
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
        cursor_style_for, display_column, display_positions, display_width,
        matching_partner_for_render, visible_range, AppMode,
    };
    use crate::app::App;
    use crossterm::cursor::SetCursorStyle;
    use wed::core::{CharPos, Mode, SelectionKind};

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
        let chars = vec!['\t', 'x'];
        assert_eq!(display_width('\t', 0), 8);
        assert_eq!(display_width('\t', 3), 5);
        assert_eq!(display_column(&chars, 1), 8);
        let positions = display_positions(&chars);
        assert_eq!(positions, vec![(0, 8), (8, 9)]);
        assert_eq!(visible_range(&positions, 3, 2), (0, 1));
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
