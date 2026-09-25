use crate::buffer::GapBuffer;
use crate::clipboard;
use crate::history::{Action, History};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    GoToLine,
    SavePrompt,
    QuitPrompt,
}

pub struct Engine {
    pub buffer: GapBuffer,
    pub history: History,
    pub cursor_char_idx: usize,
    pub mode: Mode,
    pub filepath: Option<String>,
    pub dirty: bool,
    pub prompt_input: String,
    pub search_matches: Vec<usize>,
    pub current_match_idx: usize,
    pub status_message: String,
    pub selection_start: Option<usize>,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            buffer: GapBuffer::new(),
            history: History::new(),
            cursor_char_idx: 0,
            mode: Mode::Normal,
            filepath: None,
            dirty: false,
            prompt_input: String::new(),
            search_matches: Vec::new(),
            current_match_idx: 0,
            status_message: "Welcome to wed | Ctrl+S: Save | Ctrl+Q: Quit | Ctrl+F: Find".to_string(),
            selection_start: None,
        }
    }

    pub fn load_file<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<()> {
        let path_ref = path.as_ref();
        if path_ref.exists() {
            let mut file = File::open(path_ref)?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            self.buffer = GapBuffer::from_string(&content);
        } else {
            self.buffer = GapBuffer::new();
        }
        self.filepath = Some(path_ref.to_string_lossy().into_owned());
        self.cursor_char_idx = 0;
        self.dirty = false;
        self.history.clear();
        self.selection_start = None;
        self.status_message = format!("Loaded: {}", path_ref.to_string_lossy());
        Ok(())
    }

    pub fn save_file(&mut self) -> std::io::Result<()> {
        if let Some(ref path) = self.filepath {
            let mut file = File::create(path)?;
            let content = self.buffer.to_string();
            file.write_all(content.as_bytes())?;
            self.dirty = false;
            self.status_message = format!("Saved successfully to {}", path);
            Ok(())
        } else {
            self.mode = Mode::SavePrompt;
            self.prompt_input.clear();
            Ok(())
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Search => self.handle_search_key(key),
            Mode::GoToLine => self.handle_goto_key(key),
            Mode::SavePrompt => self.handle_save_prompt_key(key),
            Mode::QuitPrompt => self.handle_quit_prompt_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('q') => {
                    if self.dirty {
                        self.mode = Mode::QuitPrompt;
                        return false;
                    }
                    return true;
                }
                KeyCode::Char('s') => {
                    let _ = self.save_file();
                }
                KeyCode::Char('z') => {
                    if let Some(action) = self.history.undo() {
                        self.apply_action(action);
                        self.dirty = true;
                    }
                }
                KeyCode::Char('y') => {
                    if let Some(action) = self.history.redo() {
                        self.apply_action(action);
                        self.dirty = true;
                    }
                }
                KeyCode::Char('f') => {
                    self.mode = Mode::Search;
                    self.prompt_input.clear();
                    self.search_matches.clear();
                }
                KeyCode::Char('g') => {
                    self.mode = Mode::GoToLine;
                    self.prompt_input.clear();
                }
                KeyCode::Char('a') => {
                    self.selection_start = Some(0);
                    self.cursor_char_idx = self.buffer.len();
                }
                KeyCode::Char('c') => {
                    if let Some(start) = self.selection_start {
                        let end = self.cursor_char_idx;
                        let s_min = std::cmp::min(start, end);
                        let s_max = std::cmp::max(start, end);
                        if s_min != s_max {
                            let text = self.buffer.get_range_string(s_min, s_max);
                            clipboard::set_contents(&text);
                        }
                    }
                }
                KeyCode::Char('v') => {
                    let _ = self.delete_selection();
                    let text = clipboard::get_contents();
                    for c in text.chars() {
                        self.buffer.insert(self.cursor_char_idx, c);
                        self.history.record(Action::Insert(self.cursor_char_idx, c));
                        self.cursor_char_idx += 1;
                    }
                    if !text.is_empty() {
                        self.dirty = true;
                    }
                }
                KeyCode::Char('x') => {
                    if let Some(start) = self.selection_start {
                        let end = self.cursor_char_idx;
                        let s_min = std::cmp::min(start, end);
                        let s_max = std::cmp::max(start, end);
                        if s_min != s_max {
                            let text = self.buffer.get_range_string(s_min, s_max);
                            clipboard::set_contents(&text);
                            let _ = self.delete_selection();
                        }
                    }
                }
                _ => {}
            }
            return false;
        }

        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
                self.handle_movement_key(key);
            }
            KeyCode::Char(c) => {
                let _ = self.delete_selection();
                self.buffer.insert(self.cursor_char_idx, c);
                self.history.record(Action::Insert(self.cursor_char_idx, c));
                self.cursor_char_idx += 1;
                self.dirty = true;
            }
            KeyCode::Enter => {
                let _ = self.delete_selection();
                self.buffer.insert(self.cursor_char_idx, '\n');
                self.history.record(Action::Insert(self.cursor_char_idx, '\n'));
                self.cursor_char_idx += 1;
                self.dirty = true;
            }
            KeyCode::Backspace => {
                if !self.delete_selection() {
                    if self.cursor_char_idx > 0 {
                        self.cursor_char_idx -= 1;
                        if let Some(c) = self.buffer.delete(self.cursor_char_idx) {
                            self.history.record(Action::Delete(self.cursor_char_idx, c));
                            self.dirty = true;
                        }
                    }
                }
            }
            KeyCode::Delete => {
                if !self.delete_selection() {
                    if self.cursor_char_idx < self.buffer.len() {
                        if let Some(c) = self.buffer.delete(self.cursor_char_idx) {
                            self.history.record(Action::Delete(self.cursor_char_idx, c));
                            self.dirty = true;
                        }
                    }
                }
            }
            _ => {}
        }
        false
    }

    fn handle_movement_key(&mut self, key: KeyEvent) {
        let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if has_shift {
            if self.selection_start.is_none() {
                self.selection_start = Some(self.cursor_char_idx);
            }
        } else {
            self.selection_start = None;
        }

        match key.code {
            KeyCode::Left => {
                if self.cursor_char_idx > 0 {
                    self.cursor_char_idx -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_char_idx < self.buffer.len() {
                    self.cursor_char_idx += 1;
                }
            }
            KeyCode::Up => {
                self.move_cursor_vertical(-1);
            }
            KeyCode::Down => {
                self.move_cursor_vertical(1);
            }
            KeyCode::Home => {
                self.cursor_char_idx = self.buffer.line_start_idx(self.cursor_char_idx);
            }
            KeyCode::End => {
                self.cursor_char_idx = self.buffer.line_end_idx(self.cursor_char_idx);
            }
            _ => {}
        }
    }

    fn delete_selection(&mut self) -> bool {
        if let Some(start) = self.selection_start {
            let end = self.cursor_char_idx;
            let s_min = std::cmp::min(start, end);
            let s_max = std::cmp::max(start, end);
            if s_min != s_max {
                for i in (s_min..s_max).rev() {
                    if let Some(c) = self.buffer.delete(i) {
                        self.history.record(Action::Delete(i, c));
                    }
                }
                self.cursor_char_idx = s_min;
                self.selection_start = None;
                self.dirty = true;
                return true;
            }
        }
        self.selection_start = None;
        false
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) => {
                self.prompt_input.push(c);
                self.update_search();
            }
            KeyCode::Backspace => {
                self.prompt_input.pop();
                self.update_search();
            }
            KeyCode::Enter => {
                if !self.search_matches.is_empty() {
                    self.current_match_idx = (self.current_match_idx + 1) % self.search_matches.len();
                    self.cursor_char_idx = self.search_matches[self.current_match_idx];
                }
            }
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.prompt_input.clear();
                self.search_matches.clear();
            }
            _ => {}
        }
        false
    }

    fn handle_goto_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.prompt_input.push(c);
            }
            KeyCode::Backspace => {
                self.prompt_input.pop();
            }
            KeyCode::Enter => {
                if let Ok(line_num) = self.prompt_input.parse::<usize>() {
                    self.cursor_char_idx = self.buffer.idx_at_line(line_num);
                }
                self.mode = Mode::Normal;
                self.prompt_input.clear();
            }
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.prompt_input.clear();
            }
            _ => {}
        }
        false
    }

    fn handle_save_prompt_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) => {
                self.prompt_input.push(c);
            }
            KeyCode::Backspace => {
                self.prompt_input.pop();
            }
            KeyCode::Enter => {
                if !self.prompt_input.trim().is_empty() {
                    self.filepath = Some(self.prompt_input.trim().to_string());
                    let _ = self.save_file();
                }
                self.mode = Mode::Normal;
                self.prompt_input.clear();
            }
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.prompt_input.clear();
            }
            _ => {}
        }
        false
    }

    fn handle_quit_prompt_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let _ = self.save_file();
                return true;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                return true;
            }
            KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('C') => {
                self.mode = Mode::Normal;
            }
            _ => {}
        }
        false
    }

    fn update_search(&mut self) {
        if self.prompt_input.is_empty() {
            return;
        }
        let matches = self.buffer.search_all(&self.prompt_input);
        if !matches.is_empty() {
            let mut matched_vec = matches;
            matched_vec.sort_unstable();
            let mut current = 0;
            for (i, &idx) in matched_vec.iter().enumerate() {
                if idx >= self.cursor_char_idx {
                    current = i;
                    break;
                }
            }
            self.search_matches = matched_vec;
            self.current_match_idx = current;
            self.cursor_char_idx = self.search_matches[current];
        }
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::Insert(idx, c) => {
                self.buffer.insert(idx, c);
                self.cursor_char_idx = idx + 1;
            }
            Action::Delete(idx, _) => {
                self.buffer.delete(idx);
                self.cursor_char_idx = idx;
            }
        }
    }

    fn move_cursor_vertical(&mut self, dir: isize) {
        let (current_line, current_col) = self.buffer.line_col_at_idx(self.cursor_char_idx);
        let target_line = if dir < 0 {
            current_line.saturating_sub((-dir) as usize)
        } else {
            current_line.saturating_add(dir as usize)
        };
        self.cursor_char_idx = self.buffer.idx_at_line_col(target_line, current_col);
    }
}