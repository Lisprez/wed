use crate::engine::{Engine, Mode};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{Clear, ClearType, size},
};
use std::io::{self, BufWriter, Write, stdout};

pub struct Viewer {
    writer: BufWriter<io::Stdout>,
    width: u16,
    height: u16,
    row_offset: usize,
    col_offset: usize,
}

impl Viewer {
    pub fn new() -> io::Result<Self> {
        let (width, height) = size()?;
        Ok(Self {
            writer: BufWriter::with_capacity(1 << 16, stdout()),
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

    pub fn render(&mut self, engine: &Engine) -> io::Result<()> {
        queue!(self.writer, Hide)?;
        
        let text_height = (self.height as usize).saturating_sub(2);
        let text_width = self.width as usize;

        let (cursor_row, cursor_col) = engine.buffer.line_col_at_idx(engine.cursor_char_idx);

        if cursor_row < self.row_offset {
            self.row_offset = cursor_row;
        }
        if cursor_row >= self.row_offset + text_height {
            self.row_offset = cursor_row - text_height + 1;
        }
        if cursor_col < self.col_offset {
            self.col_offset = cursor_col;
        }
        if cursor_col >= self.col_offset + text_width {
            self.col_offset = cursor_col - text_width + 1;
        }

        let total_lines = engine.buffer.total_lines();

        for screen_row in 0..text_height {
            let file_row = self.row_offset + screen_row;
            queue!(self.writer, MoveTo(0, screen_row as u16), Clear(ClearType::CurrentLine))?;

            if file_row < total_lines {
                let line_content = engine.buffer.get_line_string(file_row);
                let line_start_char_idx = engine.buffer.idx_at_line(file_row + 1);
                let chars: Vec<char> = line_content.chars().collect();
                if self.col_offset < chars.len() {
                    let end = std::cmp::min(self.col_offset + text_width, chars.len());
                    for (i, &c) in chars[self.col_offset..end].iter().enumerate() {
                        let char_idx = line_start_char_idx + self.col_offset + i;
                        let is_selected = if let Some(start) = engine.selection_start {
                            let s_min = std::cmp::min(start, engine.cursor_char_idx);
                            let s_max = std::cmp::max(start, engine.cursor_char_idx);
                            char_idx >= s_min && char_idx < s_max
                        } else {
                            false
                        };

                        if is_selected {
                            queue!(
                                self.writer,
                                SetBackgroundColor(Color::White),
                                SetForegroundColor(Color::Black),
                                Print(c),
                                ResetColor
                            )?;
                        } else {
                            queue!(self.writer, Print(c))?;
                        }
                    }
                }
            } else {
                queue!(self.writer, SetForegroundColor(Color::DarkGrey), Print("~"), ResetColor)?;
            }
        }

        self.render_status_bar(engine)?;
        self.render_message_bar(engine)?;

        let screen_cursor_row = (cursor_row - self.row_offset) as u16;
        let screen_cursor_col = (cursor_col - self.col_offset) as u16;
        queue!(self.writer, MoveTo(screen_cursor_col, screen_cursor_row), Show)?;

        self.writer.flush()?;
        Ok(())
    }

    fn render_status_bar(&mut self, engine: &Engine) -> io::Result<()> {
        let status_row = self.height.saturating_sub(2);
        queue!(self.writer, MoveTo(0, status_row), Clear(ClearType::CurrentLine))?;

        let dirty_indicator = if engine.dirty { "[+]" } else { "" };
        let filename = engine.filepath.as_deref().unwrap_or("[No Name]");
        let (row, col) = engine.buffer.line_col_at_idx(engine.cursor_char_idx);
        
        let left_info = format!(" wed - {}{} ", filename, dirty_indicator);
        let right_info = format!(" Line: {}, Col: {} ", row + 1, col + 1);
        
        let width = self.width as usize;
        let pad_len = width.saturating_sub(left_info.len() + right_info.len());
        let padding: String = " ".repeat(pad_len);
        
        queue!(
            self.writer,
            SetBackgroundColor(Color::White),
            SetForegroundColor(Color::Black),
            Print(format!("{}{}{}", left_info, padding, right_info)),
            ResetColor
        )?;
        Ok(())
    }

    fn render_message_bar(&mut self, engine: &Engine) -> io::Result<()> {
        let msg_row = self.height.saturating_sub(1);
        queue!(self.writer, MoveTo(0, msg_row), Clear(ClearType::CurrentLine))?;

        match engine.mode {
            Mode::Normal => {
                queue!(self.writer, Print(&engine.status_message))?;
            }
            Mode::Search => {
                let match_info = if engine.search_matches.is_empty() {
                    "0/0".to_string()
                } else {
                    format!("{}/{}", engine.current_match_idx + 1, engine.search_matches.len())
                };
                queue!(
                    self.writer,
                    SetForegroundColor(Color::Yellow),
                    Print(format!("Search: {} ({})", engine.prompt_input, match_info)),
                    ResetColor
                )?;
            }
            Mode::GoToLine => {
                queue!(
                    self.writer,
                    SetForegroundColor(Color::Cyan),
                    Print(format!("Go to line: {}", engine.prompt_input)),
                    ResetColor
                )?;
            }
            Mode::SavePrompt => {
                queue!(
                    self.writer,
                    SetForegroundColor(Color::Green),
                    Print(format!("Save as: {}", engine.prompt_input)),
                    ResetColor
                )?;
            }
            Mode::QuitPrompt => {
                queue!(
                    self.writer,
                    SetForegroundColor(Color::Red),
                    Print("Unsaved changes! Save first? (y/n/c)"),
                    ResetColor
                )?;
            }
        }
        Ok(())
    }
}