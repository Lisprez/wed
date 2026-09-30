use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use wed::clipboard;
use wed::core::{Editor, EditorOutcome, Key};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppMode {
    Editing,
    CommandLine,
    QuitPrompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppOutcome {
    Continue,
    Quit,
}

pub struct App {
    pub editor: Editor,
    path: Option<PathBuf>,
    command_input: String,
    mode: AppMode,
}

impl App {
    pub fn new(path: Option<PathBuf>) -> io::Result<Self> {
        let editor = if let Some(path) = path.as_ref() {
            if path.exists() {
                let text = fs::read_to_string(path).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("Unable to read {}: {}", path.display(), error),
                    )
                })?;
                Editor::from_text(&text)
            } else {
                Editor::new()
            }
        } else {
            Editor::new()
        };
        Ok(Self {
            editor,
            path,
            command_input: String::new(),
            mode: AppMode::Editing,
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn mode(&self) -> AppMode {
        self.mode
    }

    pub fn command_prompt(&self) -> Option<String> {
        (self.mode == AppMode::CommandLine).then(|| format!(":{}", self.command_input))
    }

    pub fn is_dirty(&self) -> bool {
        self.editor.is_dirty()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> io::Result<AppOutcome> {
        if self.mode == AppMode::QuitPrompt {
            return self.handle_quit_prompt(key);
        }
        if self.mode == AppMode::CommandLine {
            return self.handle_command_line(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('Q') => return self.request_quit(),
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.save();
                    return Ok(AppOutcome::Continue);
                }
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    self.editor.handle_key(Key::Char('u'));
                    return Ok(AppOutcome::Continue);
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Char(':')
            && self.editor.mode == wed::core::Mode::Normal
            && !self.editor.is_waiting_for_character()
        {
            self.mode = AppMode::CommandLine;
            self.command_input.clear();
            return Ok(AppOutcome::Continue);
        }
        let key = convert_key(key);
        self.handle_editor_key(key, &clipboard::set_contents)
    }

    fn handle_editor_key(
        &mut self,
        key: Key,
        set_clipboard: &impl Fn(&str) -> io::Result<()>,
    ) -> io::Result<AppOutcome> {
        let outcome = self.editor.handle_key(key);
        if let Some(text) = self.editor.take_clipboard_text() {
            if let Err(error) = set_clipboard(&text) {
                self.editor.message = format!("Clipboard copy failed: {}", error);
            }
        }
        Ok(match outcome {
            EditorOutcome::Continue => AppOutcome::Continue,
            EditorOutcome::Quit => AppOutcome::Quit,
        })
    }

    pub fn save(&mut self) -> bool {
        self.editor.commit_pending_edit();
        let Some(path) = self.path.as_ref() else {
            self.editor.message =
                "No file name. Use a file path when starting the editor.".to_string();
            return false;
        };
        let temporary = temporary_path(path);
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            self.editor.document.write_to(&mut file)?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            self.editor.message = format!("Save failed: {}", error);
            false
        } else {
            self.editor.mark_saved();
            self.editor.message = format!("Saved {}", path.display());
            true
        }
    }

    fn handle_command_line(&mut self, key: KeyEvent) -> io::Result<AppOutcome> {
        match key.code {
            KeyCode::Esc => {
                self.command_input.clear();
                self.mode = AppMode::Editing;
            }
            KeyCode::Backspace => {
                self.command_input.pop();
            }
            KeyCode::Enter => {
                let command = self.command_input.trim().to_string();
                self.command_input.clear();
                self.mode = AppMode::Editing;
                return self.execute_command(&command);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.command_input.push(character);
            }
            _ => {}
        }
        Ok(AppOutcome::Continue)
    }

    fn execute_command(&mut self, command: &str) -> io::Result<AppOutcome> {
        match command {
            "" => Ok(AppOutcome::Continue),
            "w" => {
                self.save();
                Ok(AppOutcome::Continue)
            }
            "q" => self.request_quit(),
            "q!" => Ok(AppOutcome::Quit),
            "wq" | "x" => {
                if self.save() {
                    Ok(AppOutcome::Quit)
                } else {
                    Ok(AppOutcome::Continue)
                }
            }
            _ => {
                if let Some(path) = command.strip_prefix("e ") {
                    return match self.open_path(PathBuf::from(path.trim())) {
                        Ok(outcome) => Ok(outcome),
                        Err(error) => {
                            self.editor.message = format!("Open failed: {}", error);
                            Ok(AppOutcome::Continue)
                        }
                    };
                }
                if let Ok(line) = command.parse::<usize>() {
                    self.editor.goto_line(line);
                    return Ok(AppOutcome::Continue);
                }
                if let Some((needle, replacement, global)) = parse_substitution(command) {
                    self.editor.replace_literal(&needle, &replacement, global);
                    return Ok(AppOutcome::Continue);
                }
                self.editor.message = format!("Unknown command: {}", command);
                Ok(AppOutcome::Continue)
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) -> io::Result<AppOutcome> {
        let text = fs::read_to_string(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Unable to read {}: {}", path.display(), error),
            )
        })?;
        self.editor.replace_text(&text);
        self.editor.mark_saved();
        self.path = Some(path.clone());
        self.editor.message = format!("Loaded {}", path.display());
        Ok(AppOutcome::Continue)
    }

    fn request_quit(&mut self) -> io::Result<AppOutcome> {
        if self.editor.is_dirty() {
            self.mode = AppMode::QuitPrompt;
            Ok(AppOutcome::Continue)
        } else {
            Ok(AppOutcome::Quit)
        }
    }

    fn handle_quit_prompt(&mut self, key: KeyEvent) -> io::Result<AppOutcome> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if self.save() {
                    Ok(AppOutcome::Quit)
                } else {
                    self.mode = AppMode::Editing;
                    Ok(AppOutcome::Continue)
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => Ok(AppOutcome::Quit),
            KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('C') => {
                self.mode = AppMode::Editing;
                Ok(AppOutcome::Continue)
            }
            _ => Ok(AppOutcome::Continue),
        }
    }
}

fn convert_key(key: KeyEvent) -> Key {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(character) = key.code {
            return match character.to_ascii_lowercase() {
                'r' => Key::Ctrl('r'),
                'v' => Key::Ctrl('v'),
                other => Key::Ctrl(other),
            };
        }
    }
    match key.code {
        KeyCode::Char(character) => Key::Char(character),
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Tab => Key::Tab,
        KeyCode::Esc => Key::Escape,
        _ => Key::Escape,
    }
}

fn parse_substitution(command: &str) -> Option<(String, String, bool)> {
    let body = command.strip_prefix('s')?;
    let mut characters = body.chars();
    if characters.next()? != '/' {
        return None;
    }
    let mut parts = characters.as_str().splitn(2, '/');
    let needle = parts.next()?.to_string();
    let remainder = parts.next().unwrap_or("");
    let mut tail = remainder.splitn(2, '/');
    let replacement = tail.next().unwrap_or("").to_string();
    let flags = tail.next().unwrap_or("");
    if needle.is_empty() || (!flags.is_empty() && flags != "g") {
        return None;
    }
    Some((needle, replacement, flags == "g"))
}

fn temporary_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_else(|| OsStr::new("wed")));
    name.push(format!(".{}.tmp", process::id()));
    parent.join(name)
}

#[cfg(test)]
mod tests {
    use super::{App, AppMode};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::fs;
    use wed::core::{Editor, Key};

    fn keys(editor: &mut Editor, keys: &[Key]) {
        for key in keys {
            editor.handle_key(*key);
        }
    }

    #[test]
    fn loads_and_saves_a_file() {
        let directory = std::env::temp_dir().join(format!("wed-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("sample.txt");
        fs::write(&path, "hello").unwrap();
        let mut app = App::new(Some(path.clone())).unwrap();
        assert_eq!(app.editor.text(), "hello");
        app.editor.handle_key(wed::core::Key::Char('i'));
        app.editor.handle_key(wed::core::Key::Char('!'));
        app.editor.handle_key(wed::core::Key::Escape);
        assert!(app.save());
        assert_eq!(fs::read_to_string(path).unwrap(), "!hello");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn supports_command_line_substitution() {
        let mut app = App::new(None).unwrap();
        app.editor.insert_text_for_test("one one");
        let modifiers = KeyModifiers::NONE;
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char(':'), modifiers))
                .unwrap(),
            super::AppOutcome::Continue
        );
        for character in "s/one/two/g".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), modifiers))
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, modifiers))
            .unwrap();
        assert_eq!(app.editor.text(), "two two");
    }

    #[test]
    fn substitution_is_undoable_and_leaves_the_document_clean_when_redundant() {
        let mut editor = Editor::from_text("one one");
        assert_eq!(editor.replace_literal("one", "two", true), 2);
        assert_eq!(editor.text(), "two two");
        assert!(editor.is_dirty());

        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "one one");

        // Replacing text with itself must not leave the buffer marked dirty.
        let mut editor = Editor::from_text("one one");
        editor.mark_saved();
        assert_eq!(editor.replace_literal("one", "one", true), 2);
        assert_eq!(editor.text(), "one one");
        assert!(!editor.is_dirty());
    }

    #[test]
    fn routes_word_text_objects_through_terminal_keys() {
        let mut app = App::new(None).unwrap();
        app.editor.insert_text_for_test("one two");
        app.editor.cursor = wed::core::CharPos(1);
        let modifiers = KeyModifiers::NONE;
        for character in "ciw".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), modifiers))
                .unwrap();
        }
        assert_eq!(app.editor.text(), " two");

        let mut app = App::new(None).unwrap();
        app.editor.insert_text_for_test("one two");
        app.editor.cursor = wed::core::CharPos(1);
        for character in "vaw".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), modifiers))
                .unwrap();
        }
        let selection = app.editor.selection.as_ref().unwrap();
        assert_eq!(
            app.editor
                .document
                .slice(selection.ranges(&app.editor.document)[0]),
            "one "
        );
    }

    #[test]
    fn routes_replace_target_before_command_line() {
        let mut app = App::new(None).unwrap();
        app.editor.replace_text("abc");
        for character in "r:".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.editor.text(), ":bc");
        assert_eq!(app.mode(), AppMode::Editing);
    }

    #[test]
    fn routes_character_find_target_before_command_line() {
        let mut app = App::new(None).unwrap();
        app.editor.replace_text("a:q");
        for character in "f:".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.editor.cursor, wed::core::CharPos(1));
        assert_eq!(app.mode(), AppMode::Editing);
    }

    #[test]
    fn visual_yank_updates_register_and_system_clipboard() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut app = App::new(None).unwrap();
        app.editor.replace_text("abcd");
        let copied = Rc::new(RefCell::new(Vec::new()));
        let set_clipboard = |text: &str| {
            copied.borrow_mut().push(text.to_string());
            Ok::<_, std::io::Error>(())
        };

        for key in [
            wed::core::Key::Char('v'),
            wed::core::Key::Char('l'),
            wed::core::Key::Char('y'),
        ] {
            app.handle_editor_key(key, &set_clipboard).unwrap();
        }

        assert_eq!(copied.borrow().len(), 1);
        assert_eq!(copied.borrow()[0], "ab");
        app.editor.handle_key(wed::core::Key::Char('p'));
        assert_eq!(app.editor.text(), "abcdab");
    }

    #[test]
    fn dirty_quit_enters_prompt() {
        let mut app = App::new(None).unwrap();
        app.editor.insert_text_for_test("x");
        let event = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let outcome = app.handle_key(event).unwrap();
        assert_eq!(outcome, super::AppOutcome::Continue);
        assert_eq!(app.mode(), AppMode::QuitPrompt);
    }
}
