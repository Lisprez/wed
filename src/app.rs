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
        let editor = match path.as_ref() {
            Some(path) if path.exists() => Editor::from_document(read_document(path)?),
            _ => Editor::new(),
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

    pub fn set_viewport_height(&mut self, height: usize) {
        self.editor.set_viewport_height(height);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> io::Result<AppOutcome> {
        for error in clipboard::take_errors() {
            self.editor.message = format!("Clipboard copy failed: {error}");
        }
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
        self.handle_editor_key(key, &clipboard::queue_contents)
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
        self.serve_clipboard_request();
        Ok(match outcome {
            EditorOutcome::Continue => AppOutcome::Continue,
            EditorOutcome::Quit => AppOutcome::Quit,
        })
    }

    /// Performs a clipboard read the editor asked for.
    ///
    /// Reading means running an external program, so the editor records the
    /// intent and waits. A failed read is reported and the document left exactly
    /// as it was: the plan requires that a selection is only removed once its
    /// replacement text is actually in hand.
    fn serve_clipboard_request(&mut self) {
        if self.editor.clipboard_request().is_none() {
            return;
        }
        match clipboard::get_contents() {
            Ok(text) => self.editor.supply_clipboard(text),
            Err(error) => self.editor.report_clipboard_failure(&error.to_string()),
        }
    }

    pub fn save(&mut self) -> bool {
        self.editor.commit_pending_edit();
        let Some(path) = self.path.clone() else {
            self.editor.message =
                "No file name. Use a file path when starting the editor.".to_string();
            return false;
        };
        if self.write_to(&path) {
            self.path = Some(path);
            true
        } else {
            false
        }
    }

    /// Writes the document to `path` atomically and, on success, adopts it as the
    /// file this buffer belongs to.
    ///
    /// Shares one code path with `:w` so the atomic-replace and dirty-tracking
    /// rules cannot drift apart between the two commands.
    pub fn save_as(&mut self, path: PathBuf) -> bool {
        self.editor.commit_pending_edit();
        if self.write_to(&path) {
            self.path = Some(path);
            true
        } else {
            false
        }
    }

    /// Writes to `path` through a temporary file in the same directory, then
    /// renames over the target, so a failure part way through cannot leave a
    /// half-written file behind.
    fn write_to(&mut self, path: &Path) -> bool {
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
                // `:w path` and `:saveas path` both write and then adopt the new
                // file, so a later `:w` and `:q` refer to it.
                if let Some(target) = command
                    .strip_prefix("saveas ")
                    .or_else(|| command.strip_prefix("w "))
                {
                    let target = target.trim();
                    if target.is_empty() {
                        self.editor.message = "Save failed: no file name given".to_string();
                        return Ok(AppOutcome::Continue);
                    }
                    self.save_as(PathBuf::from(target));
                    return Ok(AppOutcome::Continue);
                }
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
                if command.starts_with('s') {
                    // A pattern, so the regex subset applies. A pattern that does
                    // not parse is reported rather than treated as an unknown
                    // command, since the user plainly meant a substitution.
                    match wed::core::Substitution::parse(command) {
                        Ok(substitution) => {
                            self.editor.substitute(&substitution);
                        }
                        Err(error) => self.editor.message = error,
                    }
                    return Ok(AppOutcome::Continue);
                }
                self.editor.message = format!("Unknown command: {}", command);
                Ok(AppOutcome::Continue)
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) -> io::Result<AppOutcome> {
        self.editor.replace_document(read_document(&path)?);
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

/// Bytes read from the file at a time.
///
/// Only affects peak memory during a load, which is what it is for: reading the
/// whole file into a `String` and handing that to the rope holds two full copies
/// at once. Streaming through a small buffer holds the rope and one chunk.
const READ_CHUNK: usize = 1 << 16;

/// Reads `path` into a document.
///
/// Rejects invalid UTF-8 explicitly rather than substituting replacement
/// characters: silently rewriting a user's file on the next save is the kind of
/// data loss the plan rules out.
fn read_document(path: &Path) -> io::Result<wed::core::Document> {
    use std::io::Read;

    let mut file = fs::File::open(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Unable to read {}: {}", path.display(), error),
        )
    })?;
    let mut document = wed::core::Document::new();
    // A trailing fragment may split a multi-byte character, so it is carried over
    // rather than decoded on its own.
    let mut pending: Vec<u8> = Vec::with_capacity(READ_CHUNK);
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..read]);
        match std::str::from_utf8(&pending) {
            Ok(text) => {
                document.append_str(text);
                pending.clear();
            }
            Err(error) if error.error_len().is_none() => {
                // Incomplete tail: keep it for the next round.
                let valid = error.valid_up_to();
                if valid > 0 {
                    let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                    document.append_str(&text);
                }
                pending.drain(..valid);
            }
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not valid UTF-8: {}", path.display(), error),
                ));
            }
        }
    }
    if !pending.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} ends with an incomplete UTF-8 sequence", path.display()),
        ));
    }
    Ok(document)
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
    use super::{App, AppMode, READ_CHUNK};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::fs;
    use std::path::PathBuf;
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
    fn substitution_patterns_go_through_the_regex_subset() {
        let mut app = App::new(None).unwrap();
        app.editor.replace_text("a1 b2 c3");
        run_command(&mut app, "s/[0-9]/_/g");
        assert_eq!(app.editor.text(), "a_ b_ c_");

        run_command(&mut app, r"s/^/> /g");
        assert_eq!(app.editor.text(), "> a_ b_ c_");

        // Undo rewinds the whole substitution.
        keys(&mut app.editor, &[Key::Char('u')]);
        assert_eq!(app.editor.text(), "a_ b_ c_");
    }

    #[test]
    fn a_malformed_substitution_is_reported_rather_than_ignored() {
        let mut app = App::new(None).unwrap();
        app.editor.replace_text("hello");
        run_command(&mut app, "s/(unclosed/x/");
        assert!(
            app.editor.message.starts_with("bad pattern:"),
            "got {:?}",
            app.editor.message
        );
        assert_eq!(app.editor.text(), "hello", "the document must be untouched");

        run_command(&mut app, "s//x/");
        assert!(app.editor.message.contains("no pattern"));

        run_command(&mut app, "s/a/b/i");
        assert!(app.editor.message.contains("unsupported flag"));
    }

    #[test]
    fn a_substitution_with_any_delimiter_works() {
        let mut app = App::new(None).unwrap();
        // `:` as the delimiter, so the pattern and replacement can both hold a
        // slash without escaping. A `|` in the pattern means alternation only when
        // `|` is not itself the delimiter.
        app.editor.replace_text("path/to/file");
        run_command(&mut app, "s:to:from:");
        assert_eq!(app.editor.text(), "path/from/file");

        // With `/` as the delimiter, `|` is alternation.
        app.editor.replace_text("cat dog");
        run_command(&mut app, "s/cat|dog/pet/g");
        assert_eq!(app.editor.text(), "pet pet");
    }

    #[test]
    fn an_escaped_delimiter_stays_in_the_pattern() {
        let mut app = App::new(None).unwrap();
        app.editor.replace_text("a/b");
        run_command(&mut app, r"s/a\/b/X/");
        assert_eq!(app.editor.text(), "X");
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

    /// A scratch directory that removes itself.
    struct Sandbox {
        directory: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let mut directory = std::env::temp_dir();
            directory.push(format!("wed-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&directory);
            fs::create_dir_all(&directory).unwrap();
            Self { directory }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.directory.join(name)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    /// Types `command` into the command line and runs it.
    fn run_command(app: &mut App, command: &str) {
        let modifiers = KeyModifiers::NONE;
        app.handle_key(KeyEvent::new(KeyCode::Char(':'), modifiers))
            .unwrap();
        for character in command.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), modifiers))
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, modifiers))
            .unwrap();
    }

    #[test]
    fn save_as_writes_the_new_file_and_adopts_it() {
        let sandbox = Sandbox::new("save-as");
        let first = sandbox.path("first.txt");
        let second = sandbox.path("second.txt");
        fs::write(&first, "original").unwrap();

        let mut app = App::new(Some(first.clone())).unwrap();
        app.editor.insert_text_for_test("!");
        assert!(app.save_as(second.clone()));
        assert_eq!(fs::read_to_string(&second).unwrap(), "!original");
        assert_eq!(
            app.path(),
            Some(second.as_path()),
            "the buffer follows the file"
        );
        assert!(!app.is_dirty(), "a successful save clears the dirty flag");

        // A later plain `:w` must go to the adopted file, not the original.
        app.editor.insert_text_for_test("?");
        run_command(&mut app, "w");
        assert_eq!(fs::read_to_string(&second).unwrap(), "!?original");
        assert_eq!(fs::read_to_string(&first).unwrap(), "original");
    }

    #[test]
    fn w_to_a_path_saves_and_adopts_it() {
        let sandbox = Sandbox::new("w-path");
        let target = sandbox.path("target.txt");
        let mut app = App::new(None).unwrap();
        app.editor.insert_text_for_test("hello");
        run_command(&mut app, &format!("w {}", target.display()));
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(app.path(), Some(target.as_path()));
    }

    #[test]
    fn save_as_to_an_unwritable_path_reports_the_failure_and_stays_dirty() {
        let sandbox = Sandbox::new("save-as-fail");
        let missing = sandbox.path("no-such-directory").join("file.txt");
        let mut app = App::new(None).unwrap();
        app.editor.insert_text_for_test("hello");
        assert!(!app.save_as(missing));
        assert!(
            app.editor.message.contains("Save failed"),
            "got {:?}",
            app.editor.message
        );
        assert!(
            app.is_dirty(),
            "a failed save must not clear the dirty flag"
        );
        assert!(app.path().is_none(), "the buffer keeps its old file");
    }

    #[test]
    fn loading_streams_in_pieces_and_preserves_the_text() {
        let sandbox = Sandbox::new("stream-load");
        let path = sandbox.path("big.txt");
        // Larger than the read chunk, and with multi-byte characters placed so
        // that some of them straddle a chunk boundary.
        let mut text = String::new();
        while text.len() < READ_CHUNK * 3 {
            text.push_str("héllo 世界 世界\n");
        }
        text.push_str("tail without a newline");
        fs::write(&path, &text).unwrap();

        let app = App::new(Some(path)).unwrap();
        assert_eq!(app.editor.text(), text);
        assert_eq!(app.editor.document.len_chars(), text.chars().count());
    }

    #[test]
    fn loading_a_file_with_a_multibyte_character_at_a_chunk_boundary_keeps_it() {
        let sandbox = Sandbox::new("chunk-boundary");
        let path = sandbox.path("boundary.txt");
        // Place a three-byte character so its bytes straddle the chunk edge. The read
        // boundary is at byte 65536, so a character starting at 65534 occupies
        // 65534, 65535 and 65536: the last byte lands in the next read.
        let mut text = "a".repeat(READ_CHUNK - 2);
        text.push('界');
        text.push_str("tail");
        assert_eq!(
            (READ_CHUNK - 2) % 3,
            2,
            "the fixture only straddles the boundary when it starts two bytes early"
        );
        fs::write(&path, &text).unwrap();

        let app = App::new(Some(path)).unwrap();
        assert_eq!(app.editor.text(), text);
    }

    #[test]
    fn loading_rejects_invalid_utf8_rather_than_substituting_characters() {
        let sandbox = Sandbox::new("invalid-utf8");
        let path = sandbox.path("invalid.txt");
        // A lone continuation byte cannot start a character.
        fs::write(&path, [b'o', b'k', 0x80, b'\n']).unwrap();
        let error = match App::new(Some(path)) {
            Ok(_) => panic!("invalid UTF-8 must not load"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("not valid UTF-8"), "got {error}");
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
