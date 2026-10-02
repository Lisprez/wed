use super::document::Document;
use super::history::{Edit, History};
use super::motion::{self, Motion, MotionResult};
use super::position::{CharPos, CharRange, RangeKind};
use super::register::{Register, RegisterKind, RegisterValue, Registers};
use super::substitute::Substitution;
use super::textobject::{self, TextObject};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Visual(SelectionKind),
    OperatorPending(Operator),
    Search,
    CommandLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    Characterwise,
    Linewise,
    Blockwise { left: usize, right: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Escape,
    Ctrl(char),
}

impl Key {
    /// The character this key carries, if it carries one.
    fn as_char(self) -> Option<char> {
        match self {
            Key::Char(character) => Some(character),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
    IndentLeft,
    IndentRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorOutcome {
    Continue,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObjectPrefix {
    around: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingCommand {
    operator: Operator,
    count: usize,
    motion_count: Option<usize>,
    prefix: Option<ObjectPrefix>,
    g_pending: bool,
}

impl PendingCommand {
    fn combined_count(self) -> usize {
        self.count.saturating_mul(self.motion_count.unwrap_or(1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FindKind {
    Forward,
    Backward,
    TillForward,
    TillBackward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FindState {
    kind: FindKind,
    count: usize,
}

/// A paste that could not be completed inside the editor.
///
/// Reading the system clipboard means running an external program, which the
/// core cannot do, so a command that needs it records what it wants and waits.
/// The application layer performs the read and calls
/// [`Editor::supply_clipboard`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasteRequest {
    /// `P` rather than `p`: place the text before the cursor.
    pub before: bool,
    /// Replace the current selection rather than insert at the cursor.
    pub over_selection: bool,
    /// The register the text was read from, so a black hole paste can be
    /// reported as empty rather than mistaken for a clipboard failure.
    pub register: Register,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CharPos,
    pub active: CharPos,
    pub kind: SelectionKind,
}

impl Selection {
    pub fn ranges(&self, document: &Document) -> Vec<CharRange> {
        match self.kind {
            SelectionKind::Characterwise => {
                let range = CharRange::new(self.anchor, self.active);
                let end = range
                    .end
                    .advance(usize::from(range.end.0 < document.len_chars()));
                vec![CharRange::new(range.start, end)]
            }
            SelectionKind::Linewise => {
                let first = document
                    .pos_to_line_col(self.anchor)
                    .line
                    .min(document.pos_to_line_col(self.active).line);
                let last = document
                    .pos_to_line_col(self.anchor)
                    .line
                    .max(document.pos_to_line_col(self.active).line);
                vec![CharRange::new(
                    document.line_start(first),
                    document.line_end_with_newline(last),
                )]
            }
            SelectionKind::Blockwise { left, right } => {
                let first = document
                    .pos_to_line_col(self.anchor)
                    .line
                    .min(document.pos_to_line_col(self.active).line);
                let last = document
                    .pos_to_line_col(self.anchor)
                    .line
                    .max(document.pos_to_line_col(self.active).line);
                let mut ranges = Vec::new();
                for line in first..=last {
                    let start = document.line_start(line);
                    let line_len = document.line_end(line).0.saturating_sub(start.0);
                    let left = left.min(line_len);
                    let right = right.min(line_len);
                    if left < right {
                        ranges.push(CharRange::new(start.advance(left), start.advance(right)));
                    }
                }
                ranges
            }
        }
    }

    pub fn is_empty(&self, document: &Document) -> bool {
        self.ranges(document).is_empty()
    }
}

/// Fallback page size before the terminal reports its real height.
const DEFAULT_VIEWPORT_HEIGHT: usize = 10;

pub struct Editor {
    pub document: Document,
    pub cursor: CharPos,
    pub mode: Mode,
    pub selection: Option<Selection>,
    pub message: String,
    search_input: String,
    search_backwards: bool,
    last_search: Option<String>,
    search_buffer: String,
    pub history: History,
    revision: u64,
    saved_revision: u64,
    normal_count: Option<usize>,
    pending: Option<PendingCommand>,
    visual_prefix: Option<ObjectPrefix>,
    find_pending: Option<FindState>,
    g_pending: bool,
    last_find: Option<(FindKind, char)>,
    replace_pending: bool,
    registers: Registers,
    /// Set by `"`, waiting for the key that names the register.
    register_pending: bool,
    /// Set once a register has been named, consumed by the next command that
    /// reads or writes one.
    pending_register: Option<Register>,
    pending_clipboard_text: Option<String>,
    /// Set when the last command needs the system clipboard's contents and could
    /// not get them without leaving the editor. See [`Editor::supply_clipboard`].
    clipboard_request: Option<PasteRequest>,
    viewport_height: usize,
}

impl Editor {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            document: Document::from_text(text),
            cursor: CharPos::ZERO,
            mode: Mode::Normal,
            selection: None,
            message: String::new(),
            search_input: String::new(),
            search_backwards: false,
            last_search: None,
            search_buffer: String::new(),
            history: History::new(),
            revision: 0,
            saved_revision: 0,
            normal_count: None,
            pending: None,
            visual_prefix: None,
            find_pending: None,
            g_pending: false,
            last_find: None,
            replace_pending: false,
            registers: Registers::new(),
            register_pending: false,
            pending_register: None,
            pending_clipboard_text: None,
            clipboard_request: None,
            viewport_height: DEFAULT_VIEWPORT_HEIGHT,
        }
    }

    /// Builds an editor around an already-assembled document.
    ///
    /// Takes the document by value so a file read in pieces never has to be
    /// concatenated into one string first.
    pub fn from_document(document: Document) -> Self {
        let mut editor = Self::from_text("");
        editor.document = document;
        editor
    }

    /// Swaps in an already-assembled document, as [`Self::replace_text`] does for
    /// a string.
    pub fn replace_document(&mut self, document: Document) {
        self.document = document;
        self.cursor = CharPos::ZERO;
        self.mode = Mode::Normal;
        self.selection = None;
        self.history = History::new();
        self.revision = self.revision.saturating_add(1);
        self.saved_revision = self.revision;
        self.normal_count = None;
        self.pending = None;
        self.visual_prefix = None;
        self.find_pending = None;
        self.g_pending = false;
        self.replace_pending = false;
        self.register_pending = false;
        self.pending_register = None;
        self.clipboard_request = None;
        self.search_input.clear();
        self.last_search = None;
        self.message.clear();
        self.pending_clipboard_text = None;
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_height = height.max(1);
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Upper bound on the text undo may retain. See
    /// [`History::set_budget`](super::history::History::set_budget).
    ///
    /// Lowering it discards the oldest transactions immediately, so the existing
    /// undo history is preserved as far as the new budget allows.
    pub fn set_undo_budget(&mut self, bytes: usize) {
        self.history.set_budget(bytes);
    }

    pub fn commit_pending_edit(&mut self) {
        self.history.commit();
    }

    pub fn handle_key(&mut self, key: Key) -> EditorOutcome {
        self.pending_clipboard_text = None;
        self.clipboard_request = None;
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Insert => self.handle_insert_key(key),
            Mode::Visual(_) => self.handle_visual_key(key),
            Mode::OperatorPending(_) => self.handle_pending_key(key),
            Mode::Search => self.handle_search_key(key),
            Mode::CommandLine => EditorOutcome::Continue,
        }
    }

    pub fn text(&self) -> String {
        self.document.to_string()
    }

    pub fn take_clipboard_text(&mut self) -> Option<String> {
        self.pending_clipboard_text.take()
    }

    /// The paste the last key asked for but could not complete, if any.
    pub fn clipboard_request(&self) -> Option<PasteRequest> {
        self.clipboard_request
    }

    /// Completes a paste once the application layer has read the clipboard.
    ///
    /// `text` is treated as linewise when it ends in a line break or contains
    /// one, which is the same guess every editor makes: the clipboard carries no
    /// shape of its own, and multi-line text almost always wants its own line.
    ///
    /// An empty clipboard inserts nothing and leaves the document untouched. That
    /// is deliberately not an error, because an empty clipboard is a legitimate
    /// state and the plan requires errors to be reported, not invented.
    pub fn supply_clipboard(&mut self, text: String) {
        let Some(request) = self.clipboard_request.take() else {
            return;
        };
        if text.is_empty() {
            self.message = "Clipboard is empty".to_string();
            return;
        }
        let kind = if text.contains('\n') {
            RegisterKind::Linewise
        } else {
            RegisterKind::Characterwise
        };
        self.insert_register_text(RegisterValue::new(text, kind), request);
    }

    /// Reports that the clipboard could not be read, without touching the
    /// document. The plan requires a failed read to leave the selection alone.
    pub fn report_clipboard_failure(&mut self, error: &str) {
        self.clipboard_request = None;
        self.message = format!("Clipboard read failed: {error}");
    }

    pub fn is_waiting_for_character(&self) -> bool {
        self.find_pending.is_some()
            || self.replace_pending
            || self.g_pending
            || self.register_pending
    }

    pub fn search_prompt(&self) -> Option<String> {
        (self.mode == Mode::Search).then(|| {
            format!(
                "{}{}",
                if self.search_backwards { '?' } else { '/' },
                self.search_input
            )
        })
    }

    pub fn replace_text(&mut self, text: &str) {
        self.replace_document(Document::from_text(text));
    }

    /// `:s/needle/replacement/[g]`.
    ///
    /// One match per edit, all inside a single transaction, rather than one edit
    /// spanning the whole document. The difference is what undo has to keep: the
    /// per-match form retains only the matched regions, so on a 50 MiB document with
    /// a hundred thousand hits it stores well under a megabyte where the
    /// whole-document form stored two further copies of the file, pushing the
    /// resident size to three times the file against a documented target of one to
    /// two times.
    ///
    /// `u` still rewinds the whole substitution, because the edits share a
    /// transaction.
    ///
    /// Matches are found with the same chunked search `/` uses, so the document is
    /// never materialised, and positions are tracked in the document as it stands:
    /// each replacement changes the document's length, so the next search starts past
    /// the text just written.
    /// `:s/pattern/replacement/[g]` with a regular expression.
    ///
    /// Shares the shape of [`Self::replace_literal`]: one edit per match inside a
    /// single transaction, so undo rewinds the whole substitution while the undo
    /// record holds only the matched regions rather than two further copies of the
    /// document.
    ///
    /// The document is read whole to run the matcher over. A pattern can match across
    /// any distance -- `a.*b` on a large file -- so a chunked scan would have to
    /// handle both that and line anchors, and getting the seam wrong would silently
    /// miss matches. The text is released as soon as the match positions are known,
    /// and the result is built by editing rather than by concatenating, so the peak is
    /// the rope plus one copy of the text.
    pub fn substitute(&mut self, substitution: &Substitution) -> usize {
        let text = self.document.to_string();
        let byte_ranges = substitution.match_ranges(&text);
        if byte_ranges.is_empty() {
            self.message = format!("Pattern not found: {}", substitution.pattern());
            return 0;
        }

        // The matcher reports byte offsets; the rope is indexed in characters.
        // Walking the text once converts them, and is also where a match that turns
        // out to be the needle itself gets recognised.
        let replacement = substitution.replacement();
        let ranges = to_char_ranges(&text, &byte_ranges);
        // How many the pattern matched, which is what gets reported. It is not the
        // same as how many edits were made: a match whose text already equals the
        // replacement is skipped, so `s/one/one/g` matches three times, changes
        // nothing, and must not be reported as "pattern not found".
        let matched = ranges.len();
        if matched == 0 {
            self.message = format!("Pattern not found: {}", substitution.pattern());
            return 0;
        }
        self.history.begin();
        // Back to front, so each position stays valid as the document shifts.
        for range in ranges.iter().rev() {
            let removed = self.document.slice(*range);
            if removed == replacement {
                continue;
            }
            self.apply_known_edit(*range, removed, replacement.to_string());
        }
        self.history.commit();

        self.cursor = self.cursor.clamp(self.document.len_chars());
        self.selection = None;
        self.mode = Mode::Normal;
        self.message = format!("{matched} occurrence(s) replaced");
        matched
    }

    pub fn replace_literal(&mut self, needle: &str, replacement: &str, global: bool) -> usize {
        if needle.is_empty() {
            return 0;
        }
        let needle_chars = needle.chars().count();
        let replacement_chars = replacement.chars().count();

        let mut count = 0usize;
        let mut from = 0usize;
        let mut buffer = std::mem::take(&mut self.search_buffer);
        self.history.begin();
        while from + needle_chars <= self.document.len_chars() {
            let Some(start) = find_first_match(&self.document, needle, from, &mut buffer) else {
                break;
            };
            let range = CharRange::new(start, start.advance(needle_chars));
            // The text `find_first_match` matched is the needle itself, so the undo
            // record does not have to read it back out of the rope. Passing both
            // sides by value also lets this skip the rewrite when the replacement is
            // the needle, which is what keeps `s/one/one/g` from dirtying the buffer.
            self.apply_known_edit(range, needle.to_string(), replacement.to_string());
            count += 1;
            if !global {
                break;
            }
            // Where the next match may begin, in the document as it now stands.
            from = start.0 + replacement_chars;
        }
        self.search_buffer = buffer;
        self.history.commit();

        if count == 0 {
            self.message = format!("Pattern not found: {}", needle);
            return 0;
        }
        self.cursor = self.cursor.clamp(self.document.len_chars());
        self.selection = None;
        self.mode = Mode::Normal;
        self.message = format!("{} occurrence(s) replaced", count);
        count
    }

    pub fn goto_line(&mut self, line: usize) {
        let line = line.saturating_sub(1);
        self.cursor = self.document.line_start(line);
        self.selection = None;
        self.mode = Mode::Normal;
    }

    /// Consumes keys claimed by an in-progress multi-key sequence.
    /// Returns `Some` when `key` was fully handled and the main dispatch
    /// must be skipped.
    fn preprocess_normal_key(&mut self, key: Key) -> Option<EditorOutcome> {
        if self.handle_pending_find(key) {
            return Some(EditorOutcome::Continue);
        }
        if self.register_pending {
            self.register_pending = false;
            // A register this version does not have aborts the command. Falling
            // back to the unnamed register would run `d` in `"dd` against the
            // wrong target, so failing quietly is the safe answer.
            self.pending_register = key.as_char().and_then(Register::from_key);
            return Some(EditorOutcome::Continue);
        }
        if self.replace_pending {
            if let Key::Char(character) = key {
                self.replace_pending = false;
                self.replace_current(character);
            } else if key == Key::Escape {
                self.replace_pending = false;
            }
            return Some(EditorOutcome::Continue);
        }
        if self.g_pending {
            self.g_pending = false;
            if key == Key::Char('g') {
                let count = self.take_count();
                self.move_with(Motion::GotoFirst, count);
            }
            return Some(EditorOutcome::Continue);
        }
        if let Key::Char(character) = key {
            if self.accumulate_normal_digit(character) {
                return Some(EditorOutcome::Continue);
            }
        }
        None
    }

    fn handle_normal_key(&mut self, key: Key) -> EditorOutcome {
        if let Some(outcome) = self.preprocess_normal_key(key) {
            return outcome;
        }
        match key {
            Key::Escape => self.reset_pending(),
            Key::Char('i') => self.enter_insert(false),
            Key::Char('a') => self.enter_insert(true),
            Key::Char('A') => self.append_at_line_end(),
            Key::Char('I') => {
                self.move_with(Motion::FirstNonBlank, 1);
                self.enter_insert(false);
            }
            Key::Char('o') => self.open_line(false),
            Key::Char('O') => self.open_line(true),
            Key::Char('v') => self.start_visual(SelectionKind::Characterwise),
            Key::Char('V') => self.start_visual(SelectionKind::Linewise),
            Key::Ctrl('v') => self.start_block_visual(),
            Key::Char('d') => self.start_operator(Operator::Delete),
            Key::Char('c') => self.start_operator(Operator::Change),
            Key::Char('y') => self.start_operator(Operator::Yank),
            Key::Char('>') => self.start_operator(Operator::IndentRight),
            Key::Char('<') => self.start_operator(Operator::IndentLeft),
            Key::Char('g') => self.g_pending = true,
            Key::Char('"') => self.register_pending = true,
            Key::Char('G') => {
                let count = self.take_count();
                self.cursor = self.goto_line_target(count);
                self.selection = None;
            }
            Key::Char('x') => self.delete_current(),
            Key::Char('X') => self.delete_before(),
            Key::Char('r') => self.replace_pending = true,
            Key::Char('J') => self.join_line(),
            Key::Char('p') => self.put(false),
            Key::Char('P') => self.put(true),
            Key::Char('u') => self.undo(),
            Key::Ctrl('r') => self.redo(),
            Key::Char('/') => self.begin_search(false),
            Key::Char('?') => self.begin_search(true),
            Key::Char('n') => self.repeat_search(false),
            Key::Char('N') => self.repeat_search(true),
            Key::Char('f') => self.begin_find(FindKind::Forward),
            Key::Char('F') => self.begin_find(FindKind::Backward),
            Key::Char('t') => self.begin_find(FindKind::TillForward),
            Key::Char('T') => self.begin_find(FindKind::TillBackward),
            Key::Char(';') => {
                let count = self.take_count();
                self.repeat_find(false, count);
            }
            Key::Char(',') => {
                let count = self.take_count();
                self.repeat_find(true, count);
            }
            Key::Left => self.move_with(Motion::Left, 1),
            Key::Right => self.move_with(Motion::Right, 1),
            Key::Up => self.move_with(Motion::Up, 1),
            Key::Down => self.move_with(Motion::Down, 1),
            Key::Home => self.move_with(Motion::LineStart, 1),
            Key::End => self.move_with(Motion::LineEnd, 1),
            Key::PageUp => self.move_with(Motion::ParagraphBackward, self.viewport_lines()),
            Key::PageDown => self.move_with(Motion::ParagraphForward, self.viewport_lines()),
            _ => self.apply_motion_key(key),
        }
        EditorOutcome::Continue
    }

    fn begin_search(&mut self, backwards: bool) {
        self.mode = Mode::Search;
        self.search_backwards = backwards;
        self.search_input.clear();
        self.message.clear();
    }

    fn handle_search_key(&mut self, key: Key) -> EditorOutcome {
        match key {
            Key::Escape => {
                self.mode = Mode::Normal;
                self.search_input.clear();
            }
            Key::Char(character) => self.search_input.push(character),
            Key::Backspace => {
                self.search_input.pop();
            }
            Key::Enter => {
                let query = self.search_input.clone();
                if !query.is_empty() {
                    self.last_search = Some(query.clone());
                    let mut buffer = std::mem::take(&mut self.search_buffer);
                    let target = find_search(
                        &self.document,
                        self.cursor,
                        &query,
                        self.search_backwards,
                        &mut buffer,
                    );
                    self.search_buffer = buffer;
                    if let Some(target) = target {
                        self.cursor = target;
                        self.message.clear();
                    } else {
                        self.message = format!("Pattern not found: {}", query);
                    }
                }
                self.mode = Mode::Normal;
                self.search_input.clear();
            }
            _ => {}
        }
        EditorOutcome::Continue
    }

    fn repeat_search(&mut self, reverse: bool) {
        let Some(query) = self.last_search.clone() else {
            return;
        };
        let backwards = self.search_backwards ^ reverse;
        let mut buffer = std::mem::take(&mut self.search_buffer);
        let target = find_search(&self.document, self.cursor, &query, backwards, &mut buffer);
        self.search_buffer = buffer;
        if let Some(target) = target {
            self.cursor = target;
            self.message.clear();
        } else {
            self.message = format!("Pattern not found: {}", query);
        }
    }

    fn handle_insert_key(&mut self, key: Key) -> EditorOutcome {
        match key {
            Key::Escape => {
                self.history.commit();
                self.mode = Mode::Normal;
                self.cursor = self.cursor.clamp(self.document.len_chars());
                self.cursor = CharPos(self.cursor.0.saturating_sub(1));
            }
            Key::Char(character) => self.insert_text(&character.to_string()),
            Key::Enter => self.insert_text("\n"),
            Key::Tab => self.insert_text("\t"),
            Key::Backspace => self.delete_before(),
            Key::Delete => self.delete_current(),
            Key::Left => {
                self.cursor = CharPos(self.cursor.0.saturating_sub(1));
            }
            Key::Right => {
                self.cursor = self.cursor.advance(1).clamp(self.document.len_chars());
            }
            Key::Up => self.move_with(Motion::Up, 1),
            Key::Down => self.move_with(Motion::Down, 1),
            Key::Home => self.move_with(Motion::LineStart, 1),
            Key::End => self.move_with(Motion::LineEnd, 1),
            _ => {}
        }
        EditorOutcome::Continue
    }

    fn handle_visual_key(&mut self, key: Key) -> EditorOutcome {
        if self.handle_pending_find(key) {
            return EditorOutcome::Continue;
        }
        if self.register_pending {
            self.register_pending = false;
            self.pending_register = key.as_char().and_then(Register::from_key);
            return EditorOutcome::Continue;
        }
        if let Key::Char(character) = key {
            if self.accumulate_normal_digit(character) {
                return EditorOutcome::Continue;
            }
        }
        if let Some(prefix) = self.visual_prefix {
            if let Key::Char(character) = key {
                self.visual_prefix = None;
                if let Some(object) =
                    textobject::parse(if prefix.around { 'a' } else { 'i' }, character)
                {
                    let count = self.take_count();
                    self.extend_visual_object(object, count);
                }
                return EditorOutcome::Continue;
            }
        }
        match key {
            Key::Escape => self.reset_pending(),
            Key::Char('v') => {
                self.selection = self.selection.as_ref().map(|selection| Selection {
                    kind: SelectionKind::Characterwise,
                    ..selection.clone()
                });
                self.mode = Mode::Visual(SelectionKind::Characterwise);
            }
            Key::Char('V') => {
                self.selection = self.selection.as_ref().map(|selection| Selection {
                    kind: SelectionKind::Linewise,
                    ..selection.clone()
                });
                self.mode = Mode::Visual(SelectionKind::Linewise);
            }
            Key::Ctrl('v') => self.start_block_visual(),
            Key::Char('i') => self.visual_prefix = Some(ObjectPrefix { around: false }),
            Key::Char('a') => self.visual_prefix = Some(ObjectPrefix { around: true }),
            Key::Char('o') => {
                if let Some(selection) = &mut self.selection {
                    std::mem::swap(&mut selection.anchor, &mut selection.active);
                    self.cursor = selection.active;
                }
            }
            Key::Char('"') => self.register_pending = true,
            Key::Char('p') => self.visual_put(false),
            Key::Char('P') => self.visual_put(true),
            Key::Char('d') => self.apply_visual_operator(Operator::Delete),
            Key::Char('c') => self.apply_visual_operator(Operator::Change),
            Key::Char('y') => self.apply_visual_operator(Operator::Yank),
            Key::Char('G') => {
                let count = self.take_count();
                let target = self.goto_line_target(count);
                self.update_visual_target(target);
            }
            Key::Char('f') => self.begin_find(FindKind::Forward),
            Key::Char('F') => self.begin_find(FindKind::Backward),
            Key::Char('t') => self.begin_find(FindKind::TillForward),
            Key::Char('T') => self.begin_find(FindKind::TillBackward),
            Key::Char(';') => {
                let count = self.take_count();
                self.repeat_find(false, count);
            }
            Key::Char(',') => {
                let count = self.take_count();
                self.repeat_find(true, count);
            }
            Key::Left => self.update_visual(Motion::Left),
            Key::Right => self.update_visual(Motion::Right),
            Key::Up => self.update_visual(Motion::Up),
            Key::Down => self.update_visual(Motion::Down),
            Key::Home => self.update_visual(Motion::LineStart),
            Key::End => self.update_visual(Motion::LineEnd),
            Key::Char('h') => self.update_visual(Motion::Left),
            Key::Char('j') => self.update_visual(Motion::Down),
            Key::Char('k') => self.update_visual(Motion::Up),
            Key::Char('l') => self.update_visual(Motion::Right),
            Key::Char('0') => self.update_visual(Motion::LineStart),
            Key::Char('$') => self.update_visual(Motion::LineEnd),
            Key::Char('w') => self.update_visual(Motion::WordForward),
            Key::Char('b') => self.update_visual(Motion::WordBackward),
            Key::Char('e') => self.update_visual(Motion::WordEnd),
            Key::Char('%') => self.update_visual(Motion::MatchingPair),
            _ => {}
        }
        EditorOutcome::Continue
    }

    fn handle_pending_key(&mut self, key: Key) -> EditorOutcome {
        if self.handle_pending_find(key) {
            return EditorOutcome::Continue;
        }
        let Some(pending) = self.pending else {
            self.mode = Mode::Normal;
            return EditorOutcome::Continue;
        };
        if pending.prefix.is_some() {
            return self.handle_pending_text_object(pending, key);
        }
        let Key::Char(character) = key else {
            self.cancel_pending();
            return EditorOutcome::Continue;
        };
        if self.handle_pending_digit(pending, character) {
            return EditorOutcome::Continue;
        }
        if pending.g_pending {
            return self.handle_pending_g(pending, character);
        }
        if let Some(outcome) = self.handle_pending_find_key(pending, character) {
            return outcome;
        }
        if operator_for_key(character) == Some(pending.operator) {
            self.pending = None;
            self.apply_line_operator(pending.operator, pending.count);
            return EditorOutcome::Continue;
        }
        if character == 'i' || character == 'a' {
            self.pending = Some(PendingCommand {
                prefix: Some(ObjectPrefix {
                    around: character == 'a',
                }),
                ..pending
            });
            return EditorOutcome::Continue;
        }
        if character == 'g' {
            self.pending = Some(PendingCommand {
                g_pending: true,
                ..pending
            });
            return EditorOutcome::Continue;
        }
        if character == 'G' {
            self.pending = None;
            self.apply_line_motion(pending.operator, pending.combined_count(), Motion::GotoLast);
            return EditorOutcome::Continue;
        }
        if let Some(motion) = motion_for_key(character) {
            self.pending = None;
            let count = pending.combined_count();
            let result = motion::resolve(&self.document, self.cursor, motion, count);
            self.apply_motion_result(pending.operator, result, count);
            return EditorOutcome::Continue;
        }
        self.cancel_pending();
        EditorOutcome::Continue
    }

    fn handle_pending_text_object(&mut self, pending: PendingCommand, key: Key) -> EditorOutcome {
        let Some(prefix) = pending.prefix else {
            return EditorOutcome::Continue;
        };
        if let Key::Char(character) = key {
            if let Some(object) =
                textobject::parse(if prefix.around { 'a' } else { 'i' }, character)
            {
                self.pending = None;
                self.apply_text_object(pending.operator, object, pending.combined_count());
            } else {
                self.pending = None;
                self.mode = Mode::Normal;
                self.message = "Unknown text object".to_string();
            }
        }
        EditorOutcome::Continue
    }

    fn handle_pending_digit(&mut self, pending: PendingCommand, character: char) -> bool {
        if !character.is_ascii_digit() || (character == '0' && pending.motion_count.is_none()) {
            return false;
        }
        let digit = character.to_digit(10).unwrap_or(0) as usize;
        self.pending = Some(PendingCommand {
            motion_count: Some(
                pending
                    .motion_count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit),
            ),
            ..pending
        });
        true
    }

    fn handle_pending_g(&mut self, pending: PendingCommand, character: char) -> EditorOutcome {
        self.pending = None;
        if character == 'g' {
            self.apply_line_motion(pending.operator, pending.count, Motion::GotoFirst);
        } else {
            self.mode = Mode::Normal;
        }
        EditorOutcome::Continue
    }

    fn handle_pending_find_key(
        &mut self,
        pending: PendingCommand,
        character: char,
    ) -> Option<EditorOutcome> {
        let count = pending.combined_count();
        if let Some(kind) = find_kind_for_key(character) {
            self.begin_find_with_count(kind, count);
            return Some(EditorOutcome::Continue);
        }
        if character == ';' || character == ',' {
            self.repeat_find(character == ',', count);
            return Some(EditorOutcome::Continue);
        }
        None
    }

    fn cancel_pending(&mut self) {
        self.pending = None;
        self.mode = Mode::Normal;
    }

    fn apply_motion_key(&mut self, key: Key) {
        let motion = match key {
            Key::Char('h') => Some(Motion::Left),
            Key::Char('j') => Some(Motion::Down),
            Key::Char('k') => Some(Motion::Up),
            Key::Char('l') => Some(Motion::Right),
            Key::Char('0') => Some(Motion::LineStart),
            Key::Char('^') => Some(Motion::FirstNonBlank),
            Key::Char('$') => Some(Motion::LineEnd),
            Key::Char('w') => Some(Motion::WordForward),
            Key::Char('W') => Some(Motion::WordForwardBig),
            Key::Char('b') => Some(Motion::WordBackward),
            Key::Char('B') => Some(Motion::WordBackwardBig),
            Key::Char('e') => Some(Motion::WordEnd),
            Key::Char('E') => Some(Motion::WordEndBig),
            Key::Char('(') => Some(Motion::SentenceBackward),
            Key::Char(')') => Some(Motion::SentenceForward),
            Key::Char('{') => Some(Motion::ParagraphBackward),
            Key::Char('}') => Some(Motion::ParagraphForward),
            Key::Char('%') => Some(Motion::MatchingPair),
            _ => None,
        };
        if let Some(motion) = motion {
            let count = self.take_count();
            self.move_with(motion, count);
        }
    }

    fn move_with(&mut self, motion: Motion, count: usize) {
        let result = motion::resolve(&self.document, self.cursor, motion, count);
        self.cursor = result.target;
        self.selection = None;
    }

    fn goto_line_target(&self, count: usize) -> CharPos {
        let line = if count <= 1 {
            self.document.line_count().saturating_sub(1)
        } else {
            count - 1
        };
        self.document.line_start(line)
    }

    fn update_visual(&mut self, motion: Motion) {
        let count = self.take_count();
        let Some(active) = self.selection.as_ref().map(|selection| selection.active) else {
            return;
        };
        let result = motion::resolve(&self.document, active, motion, count);
        self.update_visual_target(result.target);
    }

    fn update_visual_target(&mut self, target: CharPos) {
        // Only a block selection cares about columns, so the other kinds skip
        // the two line lookups this would otherwise cost on every motion.
        let block_columns = match self.selection.as_ref() {
            Some(selection) if matches!(selection.kind, SelectionKind::Blockwise { .. }) => {
                let column = self.document.pos_to_line_col(target).column;
                let anchor_column = self
                    .selection
                    .as_ref()
                    .map(|selection| self.document.pos_to_line_col(selection.anchor).column)
                    .unwrap_or(column);
                Some((anchor_column, column))
            }
            _ => None,
        };
        if let Some(selection) = &mut self.selection {
            selection.active = target;
            if let SelectionKind::Blockwise { left, right } = &mut selection.kind {
                let (anchor_column, column) = block_columns.unwrap_or((0, 0));
                *left = anchor_column.min(column);
                *right = anchor_column.max(column) + 1;
            }
        }
        self.cursor = target;
    }

    fn begin_find(&mut self, kind: FindKind) {
        let count = self.take_count();
        self.begin_find_with_count(kind, count);
    }

    fn begin_find_with_count(&mut self, kind: FindKind, count: usize) {
        self.find_pending = Some(FindState {
            kind,
            count: count.max(1),
        });
    }

    fn start_operator(&mut self, operator: Operator) {
        let count = self.take_count();
        self.pending = Some(PendingCommand {
            operator,
            count,
            motion_count: None,
            prefix: None,
            g_pending: false,
        });
        self.mode = Mode::OperatorPending(operator);
    }

    fn start_visual(&mut self, kind: SelectionKind) {
        let count = self.take_count();
        if count > 1 {
            self.update_visual(Motion::Down);
        }
        self.selection = Some(Selection {
            anchor: self.cursor,
            active: self.cursor,
            kind,
        });
        self.mode = Mode::Visual(kind);
    }

    fn start_block_visual(&mut self) {
        let column = self.document.pos_to_line_col(self.cursor).column;
        self.selection = Some(Selection {
            anchor: self.cursor,
            active: self.cursor,
            kind: SelectionKind::Blockwise {
                left: column,
                right: column + 1,
            },
        });
        self.mode = Mode::Visual(SelectionKind::Blockwise {
            left: column,
            right: column + 1,
        });
    }

    fn append_at_line_end(&mut self) {
        let line = self.document.pos_to_line_col(self.cursor).line;
        self.cursor = self.document.line_end(line);
        self.enter_insert(false);
    }

    fn enter_insert(&mut self, after: bool) {
        self.history.begin();
        if after {
            let line = self.document.pos_to_line_col(self.cursor).line;
            let end = self.document.line_end(line);
            self.cursor = if self.cursor.0 < end.0 {
                self.cursor.advance(1)
            } else {
                end
            };
        }
        self.mode = Mode::Insert;
        self.selection = None;
    }

    fn open_line(&mut self, before: bool) {
        let line = self.document.pos_to_line_col(self.cursor).line;
        let position = if before {
            self.document.line_start(line)
        } else {
            self.document.line_end_with_newline(line)
        };
        self.history.begin();
        self.apply_edit(CharRange::empty(position), "\n");
        self.cursor = position.advance(1);
        self.mode = Mode::Insert;
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let start = self.cursor;
        self.apply_edit(CharRange::empty(start), text);
        self.cursor = start.advance(text.chars().count());
    }

    fn delete_current(&mut self) {
        if self.cursor.0 >= self.document.len_chars() {
            return;
        }
        let range = CharRange::new(self.cursor, self.cursor.advance(1));
        self.apply_edit(range, "");
    }

    fn delete_before(&mut self) {
        if self.cursor.0 == 0 {
            return;
        }
        let range = CharRange::new(CharPos(self.cursor.0 - 1), self.cursor);
        self.apply_edit(range, "");
        self.cursor = CharPos(self.cursor.0.saturating_sub(1));
    }

    fn replace_current(&mut self, character: char) {
        if self.cursor.0 >= self.document.len_chars() {
            self.insert_text(&character.to_string());
            return;
        }
        let range = CharRange::new(self.cursor, self.cursor.advance(1));
        self.apply_edit(range, &character.to_string());
    }

    fn join_line(&mut self) {
        let line = self.document.pos_to_line_col(self.cursor).line;
        if line + 1 >= self.document.line_count() {
            return;
        }
        let end = self.document.line_end(line);
        let next_start = self.document.line_end_with_newline(line);
        if next_start.0 <= end.0 {
            return;
        }
        self.apply_edit(CharRange::new(end, next_start), " ");
        self.cursor = end;
    }

    /// `p` and `P`, for the register a `"` prefix named.
    ///
    /// The system clipboard cannot be read from here, so that case records a
    /// request and lets [`Editor::supply_clipboard`] finish the job. The plan
    /// requires the read to happen before anything is removed, so this never
    /// touches the document on the clipboard path.
    fn put(&mut self, before: bool) {
        let register = self.take_register();
        if register.needs_external_read() {
            self.clipboard_request = Some(PasteRequest {
                before,
                over_selection: false,
                register,
            });
            return;
        }
        let Some(value) = self.read_register(register) else {
            self.message = "Register is empty".to_string();
            return;
        };
        self.insert_register_text(
            value,
            PasteRequest {
                before,
                over_selection: false,
                register,
            },
        );
    }

    /// Reads a register the editor holds. The black hole always reads empty.
    fn read_register(&self, register: Register) -> Option<RegisterValue> {
        match register {
            Register::Unnamed => self.registers.unnamed().cloned(),
            // The black hole holds nothing by definition.
            Register::BlackHole => None,
            // Handled by the application layer; see `Editor::clipboard_request`.
            Register::Clipboard => None,
        }
    }

    /// Inserts register text, either at the cursor or over the selection.
    ///
    /// Replacing a selection is one undo step, not one for the deletion and
    /// another for the insertion, so the whole thing is wrapped in a single
    /// transaction here.
    fn insert_register_text(&mut self, value: RegisterValue, request: PasteRequest) {
        if request.over_selection {
            let Some(selection) = self.selection.clone() else {
                return;
            };
            let mut ranges = selection.ranges(&self.document);
            // Linewise text replaces whole lines, so a selection that covered only
            // part of a row is widened to the rows it touched. Without this the
            // pasted lines would stack up next to the line breaks the characterwise
            // selection left behind.
            if value.is_linewise() {
                if let (Some(first), Some(last)) = (ranges.first(), ranges.last()) {
                    ranges = vec![self.linewise_range(CharRange::new(first.start, last.end))];
                }
            }
            let Some(first) = ranges.first().copied() else {
                return;
            };
            let anchor = first.start;
            self.selection = None;
            self.history.begin();
            for range in ranges.iter().rev() {
                self.apply_edit(*range, "");
            }
            self.write_paste_text(value, anchor);
            self.history.commit();
            self.mode = Mode::Normal;
            return;
        }
        self.history.begin();
        self.insert_at_cursor(value, request.before, self.cursor);
        self.history.commit();
    }

    /// Writes pasted text at exactly `position`.
    ///
    /// No line or word adjustment, because replacing a selection means the new
    /// text starts where the old text did. Applying `p` semantics here would
    /// push the text past the end of the row the selection sat on.
    fn write_paste_text(&mut self, value: RegisterValue, position: CharPos) {
        let linewise = value.is_linewise();
        let mut text = value.text;
        if linewise && !text.ends_with('\n') {
            text.push('\n');
        }
        self.apply_edit(CharRange::empty(position), &text);
        self.cursor = position.advance(text.chars().count().saturating_sub(1));
    }

    /// Places register text relative to `anchor`, and leaves the cursor on its
    /// last character. The caller owns the surrounding transaction.
    fn insert_at_cursor(&mut self, value: RegisterValue, before: bool, anchor: CharPos) {
        let linewise = value.is_linewise();
        let mut text = value.text;
        let position = if linewise {
            let line = self.document.pos_to_line_col(anchor).line;
            if before {
                self.document.line_start(line)
            } else {
                self.document.line_end_with_newline(line)
            }
        } else {
            let position = self.characterwise_put_position(before, anchor);
            if !before
                && position.0 < self.document.len_chars()
                && self
                    .document
                    .char_at(position)
                    .is_some_and(|character| matches!(character, ' ' | '\t'))
            {
                text.insert(0, ' ');
            }
            position
        };
        // A linewise paste that arrives without a trailing break would otherwise
        // run into the line it was put on.
        if linewise && !text.ends_with('\n') {
            text.push('\n');
        }
        // On the last line of a file that does not end in a break there is nothing
        // after it, so `p` would append to that line and run the two together. The
        // pasted lines get a line of their own instead. `position > 0` keeps an
        // empty buffer from gaining a leading blank line, where there is no
        // preceding text to separate from.
        if linewise
            && !before
            && position.0 > 0
            && position.0 == self.document.len_chars()
            && !self.document.ends_with_newline()
        {
            text.insert(0, '\n');
        }
        self.apply_edit(CharRange::empty(position), &text);
        self.cursor = position.advance(text.chars().count().saturating_sub(1));
    }

    /// `p` and `P` in Visual mode: replace the selection.
    ///
    /// As with the Normal-mode path, a clipboard paste records a request and
    /// waits, so the selection is only removed once the text is in hand.
    fn visual_put(&mut self, before: bool) {
        let register = self.take_register();
        if register.needs_external_read() {
            self.clipboard_request = Some(PasteRequest {
                before,
                over_selection: true,
                register,
            });
            return;
        }
        let Some(value) = self.read_register(register) else {
            self.message = "Register is empty".to_string();
            self.reset_pending();
            return;
        };
        self.insert_register_text(
            value,
            PasteRequest {
                before,
                over_selection: true,
                register,
            },
        );
    }

    fn characterwise_put_position(&self, before: bool, anchor: CharPos) -> CharPos {
        if before || anchor.0 >= self.document.len_chars() {
            return anchor;
        }
        let character = self.document.char_at(anchor);
        if character.is_some_and(char::is_whitespace) {
            return anchor.advance(1).clamp(self.document.len_chars());
        }
        let mut end = anchor.advance(1);
        let word = character.is_some_and(|value| value.is_alphanumeric() || value == '_');
        while end.0 < self.document.len_chars()
            && self.document.char_at(end).is_some_and(|value| {
                if word {
                    value.is_alphanumeric() || value == '_'
                } else {
                    value == character.unwrap_or_default()
                }
            })
        {
            end = end.advance(1);
        }
        end
    }

    fn apply_motion_result(&mut self, operator: Operator, result: MotionResult, count: usize) {
        let range = if result.kind == RangeKind::Linewise {
            self.linewise_range(result.range)
        } else {
            result.range.clamp(self.document.len_chars())
        };
        self.apply_operator_range(operator, range, result.kind, count);
    }

    fn apply_line_motion(&mut self, operator: Operator, count: usize, motion: Motion) {
        let current_line = self.document.pos_to_line_col(self.cursor).line;
        let target_line = match motion {
            Motion::GotoFirst => 0,
            Motion::GotoLast => self.document.line_count().saturating_sub(1),
            _ => current_line,
        };
        let first_line = current_line.min(target_line);
        let last_line = current_line.max(target_line);
        let range = CharRange::new(
            self.document.line_start(first_line),
            self.document.line_end_with_newline(last_line),
        );
        self.apply_operator_range(operator, range, RangeKind::Linewise, count);
    }

    fn apply_text_object(&mut self, operator: Operator, object: TextObject, count: usize) {
        let Some(range) = textobject::resolve(&self.document, self.cursor, object, count) else {
            self.mode = Mode::Normal;
            self.message = "Text object not found".to_string();
            return;
        };
        self.apply_operator_range(operator, range, RangeKind::Characterwise, count);
    }

    fn apply_line_operator(&mut self, operator: Operator, count: usize) {
        let line = self.document.pos_to_line_col(self.cursor).line;
        let last_line =
            (line + count.saturating_sub(1)).min(self.document.line_count().saturating_sub(1));
        let range = CharRange::new(
            self.document.line_start(line),
            self.document.line_end_with_newline(last_line),
        );
        self.apply_operator_range(operator, range, RangeKind::Linewise, count);
    }

    fn apply_operator_range(
        &mut self,
        operator: Operator,
        range: CharRange,
        kind: RangeKind,
        count: usize,
    ) {
        if operator == Operator::IndentLeft || operator == Operator::IndentRight {
            self.apply_indent(range, operator == Operator::IndentRight);
            self.pending = None;
            self.normal_count = None;
            self.mode = Mode::Normal;
            self.selection = None;
            return;
        }
        let text = self.document.slice(range);
        let text_len = text.chars().count();
        let register = self.take_register();
        match operator {
            Operator::Yank => {
                if !range.is_empty() {
                    // A yank has always mirrored to the system clipboard in wed,
                    // which keeps the text available to other programs without a
                    // `"+y`. Preserved here so adding registers does not take it
                    // away.
                    self.write_register(
                        register,
                        RegisterValue {
                            text: text.clone(),
                            kind: register_kind(kind),
                        },
                        true,
                    );
                    self.message = format!("Yanked {} characters", text_len);
                }
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Operator::Delete => {
                if !range.is_empty() {
                    self.write_register(
                        register,
                        RegisterValue {
                            text: text.clone(),
                            kind: register_kind(kind),
                        },
                        false,
                    );
                }
                self.apply_known_edit(range, text, String::new());
                self.cursor = range.start;
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Operator::Change => {
                if !range.is_empty() {
                    self.write_register(
                        register,
                        RegisterValue {
                            text: text.clone(),
                            kind: register_kind(kind),
                        },
                        false,
                    );
                }
                self.history.begin();
                self.apply_known_edit(range, text, String::new());
                self.cursor = range.start;
                self.mode = Mode::Insert;
                self.selection = None;
            }
            Operator::IndentLeft | Operator::IndentRight => unreachable!(),
        }
        self.pending = None;
        self.normal_count = None;
        let _ = count;
    }

    /// Stores `value` in `target`.
    ///
    /// `mirror` sends the text to the system clipboard as well, which is what a
    /// yank does so the copy reaches other programs. The clipboard register is
    /// never held in memory: it belongs to the operating system, and a later
    /// `"+p` reads it back from there.
    fn write_register(&mut self, target: Register, value: RegisterValue, mirror: bool) {
        if target == Register::BlackHole {
            return;
        }
        if target.is_external() || mirror {
            self.pending_clipboard_text = Some(value.text.clone());
        }
        if !target.is_external() {
            self.registers.write(target, value);
        }
    }

    fn apply_visual_operator(&mut self, operator: Operator) {
        let Some(selection) = self.selection.clone() else {
            return;
        };
        let ranges = selection.ranges(&self.document);
        if ranges.is_empty() {
            self.normal_count = None;
            self.visual_prefix = None;
            self.mode = Mode::Normal;
            self.selection = None;
            return;
        }
        let register = self.take_register();
        let register_kind = register_kind(match selection.kind {
            SelectionKind::Characterwise => RangeKind::Characterwise,
            SelectionKind::Linewise => RangeKind::Linewise,
            SelectionKind::Blockwise { .. } => RangeKind::Blockwise,
        });
        match operator {
            Operator::Yank => {
                let mut text = String::new();
                for (index, range) in ranges.iter().enumerate() {
                    if index > 0 {
                        text.push('\n');
                    }
                    self.document.write_slice(*range, &mut text);
                }
                let characters = text.chars().count();
                // Mirrored to the system clipboard for the same reason an
                // operator yank is: a copy has to reach other programs.
                self.write_register(
                    register,
                    RegisterValue {
                        text,
                        kind: register_kind,
                    },
                    true,
                );
                self.message = format!("Yanked {characters} characters");
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Operator::Delete | Operator::Change => {
                // Collect the text before the ranges are removed, so a delete
                // leaves the register holding what it took. Blockwise selections
                // join their rows with a line break, matching how they are yanked.
                let mut text = String::new();
                for (index, range) in ranges.iter().enumerate() {
                    if index > 0 {
                        text.push('\n');
                    }
                    self.document.write_slice(*range, &mut text);
                }
                if !text.is_empty() {
                    self.write_register(
                        register,
                        RegisterValue {
                            text,
                            kind: register_kind,
                        },
                        false,
                    );
                }
                // Every range in one selection is a single user action, so it has
                // to land as one transaction: a blockwise delete of three lines
                // takes one `u`, not three. Opening the transaction unconditionally
                // is safe because `begin` is a no-op when one is already open,
                // and closing it here cannot truncate an outer one -- every other
                // caller of `begin` balances it with a `commit`.
                //
                // A change deliberately leaves the transaction open, so the text
                // typed next joins the deletion; Insert mode commits on Escape.
                self.history.begin();
                for range in ranges.iter().rev() {
                    self.apply_edit(*range, "");
                }
                if operator == Operator::Change {
                    self.mode = Mode::Insert;
                } else {
                    self.history.commit();
                    self.mode = Mode::Normal;
                }
                self.cursor = ranges
                    .first()
                    .map(|range| range.start)
                    .unwrap_or(self.cursor);
                self.selection = None;
            }
            Operator::IndentLeft | Operator::IndentRight => {
                self.apply_indent(
                    CharRange::new(ranges[0].start, ranges[ranges.len() - 1].end),
                    operator == Operator::IndentRight,
                );
                self.mode = Mode::Normal;
                self.selection = None;
            }
        }
        self.normal_count = None;
        self.visual_prefix = None;
    }

    fn extend_visual_object(&mut self, object: TextObject, count: usize) {
        let Some(selection) = &mut self.selection else {
            return;
        };
        let Some(range) = textobject::resolve(&self.document, selection.active, object, count)
        else {
            return;
        };
        if matches!(object, TextObject::Paragraph { .. }) {
            selection.kind = SelectionKind::Linewise;
        }
        if range.is_empty() {
            selection.anchor = range.start;
            selection.active = range.start;
        } else {
            selection.anchor = selection.anchor.min(range.start);
            selection.active = selection.active.max(CharPos(range.end.0.saturating_sub(1)));
        }
        self.cursor = selection.active;
        let kind = selection.kind;
        self.mode = Mode::Visual(kind);
    }

    fn apply_indent(&mut self, range: CharRange, increase: bool) {
        let first_line = self.document.pos_to_line_col(range.start).line;
        let last_line = self
            .document
            .pos_to_line_col(CharPos(range.end.0.saturating_sub(1)))
            .line;
        let line_range = CharRange::new(
            self.document.line_start(first_line),
            self.document.line_end_with_newline(last_line),
        );
        let mut updated = String::new();
        for line in first_line..=last_line {
            let line_text = self.document.line_text(line);
            if increase {
                updated.push_str("    ");
                updated.push_str(&line_text);
            } else {
                let mut removed = 0;
                let mut chars = line_text.chars();
                while removed < 4 {
                    match chars.next() {
                        Some(' ') | Some('\t') => removed += 1,
                        _ => break,
                    }
                }
                updated.push_str(&line_text.chars().skip(removed).collect::<String>());
            }
            if line < last_line {
                updated.push('\n');
            }
        }
        self.apply_edit(line_range, &updated);
    }

    fn apply_edit(&mut self, range: CharRange, inserted: &str) {
        let removed = self.document.replace_range(range, inserted);
        self.record_edit(range, removed, inserted);
    }

    /// [`Self::apply_edit`] for callers that already hold the text `range`
    /// covers, so the rope is not read back out just to build the undo record.
    ///
    /// Both sides are taken by value: the undo record needs them anyway, so
    /// there is no reason to copy either one again.
    fn apply_known_edit(&mut self, range: CharRange, removed: String, inserted: String) {
        let redundant = removed == inserted || (removed.is_empty() && inserted.is_empty());
        if redundant {
            // Swapping text for itself leaves the document as it is, so there is
            // no reason to rewrite the rope at all.
            return;
        }
        self.document.overwrite_range(range, &inserted);
        self.history.record(Edit {
            start: range.start,
            removed,
            inserted,
        });
        self.revision = self.revision.saturating_add(1);
    }

    fn record_edit(&mut self, range: CharRange, removed: String, inserted: &str) {
        if removed.is_empty() && inserted.is_empty() {
            return;
        }
        if removed == inserted {
            return;
        }
        self.history.record(Edit {
            start: range.start,
            removed,
            inserted: inserted.to_string(),
        });
        self.revision = self.revision.saturating_add(1);
    }

    fn undo(&mut self) {
        if let Some(cursor) = self.history.undo(&mut self.document) {
            self.cursor = cursor;
            self.revision = self.revision.saturating_add(1);
            self.mode = Mode::Normal;
            self.selection = None;
        }
    }

    fn redo(&mut self) {
        if let Some(cursor) = self.history.redo(&mut self.document) {
            self.cursor = cursor;
            self.revision = self.revision.saturating_add(1);
            self.mode = Mode::Normal;
            self.selection = None;
        }
    }

    fn handle_pending_find(&mut self, key: Key) -> bool {
        let Some(state) = self.find_pending.take() else {
            return false;
        };
        match key {
            Key::Char(character) => self.resolve_find(state, character),
            Key::Escape => self.reset_pending(),
            _ => self.cancel_pending_find(),
        }
        true
    }

    fn resolve_find(&mut self, state: FindState, character: char) {
        self.last_find = Some((state.kind, character));
        let origin = self
            .selection
            .as_ref()
            .map(|selection| selection.active)
            .unwrap_or(self.cursor);
        let target = find_target(
            &self.document,
            origin,
            state.kind,
            character,
            state.count,
            false,
        );
        self.apply_find_target(target, state.kind, state.count);
    }

    fn apply_find_target(&mut self, target: Option<CharPos>, kind: FindKind, count: usize) {
        match self.mode {
            Mode::Normal => {
                if let Some(target) = target {
                    self.cursor = target;
                    self.message.clear();
                } else {
                    self.message = "Character not found".to_string();
                }
            }
            Mode::Visual(_) => {
                if let Some(target) = target {
                    self.update_visual_target(target);
                    self.message.clear();
                } else {
                    self.message = "Character not found".to_string();
                }
            }
            Mode::OperatorPending(_) => {
                let Some(pending) = self.pending.take() else {
                    self.mode = Mode::Normal;
                    return;
                };
                if let Some(target) = target {
                    let range = find_operator_range(&self.document, self.cursor, target, kind);
                    self.apply_operator_range(
                        pending.operator,
                        range,
                        RangeKind::Characterwise,
                        count,
                    );
                } else {
                    self.normal_count = None;
                    self.mode = Mode::Normal;
                    self.message = "Character not found".to_string();
                }
            }
            Mode::Insert | Mode::Search | Mode::CommandLine => self.reset_pending(),
        }
    }

    fn repeat_find(&mut self, reverse: bool, count: usize) {
        let Some((kind, character)) = self.last_find else {
            self.message = "No previous character search".to_string();
            self.cancel_pending_find();
            return;
        };
        let kind = if reverse {
            reverse_find_kind(kind)
        } else {
            kind
        };
        let skip_adjacent = count == 1 && is_till_find_kind(kind);
        let origin = self
            .selection
            .as_ref()
            .map(|selection| selection.active)
            .unwrap_or(self.cursor);
        let target = find_target(
            &self.document,
            origin,
            kind,
            character,
            count,
            skip_adjacent,
        );
        self.apply_find_target(target, kind, count);
    }

    fn cancel_pending_find(&mut self) {
        self.normal_count = None;
        if matches!(self.mode, Mode::OperatorPending(_)) {
            self.pending = None;
            self.mode = Mode::Normal;
        }
    }

    fn accumulate_normal_digit(&mut self, character: char) -> bool {
        if !character.is_ascii_digit() || (character == '0' && self.normal_count.is_none()) {
            return false;
        }
        let digit = character.to_digit(10).unwrap_or(0) as usize;
        self.normal_count = Some(
            self.normal_count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(digit),
        );
        true
    }

    fn take_count(&mut self) -> usize {
        self.normal_count.take().unwrap_or(1).max(1)
    }

    /// The register the next command should use, consuming a `"` prefix.
    fn take_register(&mut self) -> Register {
        self.pending_register.take().unwrap_or(Register::Unnamed)
    }

    fn reset_pending(&mut self) {
        self.normal_count = None;
        self.pending = None;
        self.visual_prefix = None;
        self.find_pending = None;
        self.g_pending = false;
        self.register_pending = false;
        self.pending_register = None;
        self.search_input.clear();
        self.replace_pending = false;
        self.selection = None;
        self.mode = Mode::Normal;
    }

    fn linewise_range(&self, range: CharRange) -> CharRange {
        let first = self.document.pos_to_line_col(range.start).line;
        let last = self
            .document
            .pos_to_line_col(CharPos(range.end.0.saturating_sub(1)))
            .line;
        CharRange::new(
            self.document.line_start(first),
            self.document.line_end_with_newline(last),
        )
    }

    fn viewport_lines(&self) -> usize {
        self.viewport_height
    }

    pub fn insert_text_for_test(&mut self, text: &str) {
        self.history.begin();
        self.insert_text(text);
        self.history.commit();
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

fn operator_for_key(character: char) -> Option<Operator> {
    match character {
        'd' => Some(Operator::Delete),
        'c' => Some(Operator::Change),
        'y' => Some(Operator::Yank),
        '>' => Some(Operator::IndentRight),
        '<' => Some(Operator::IndentLeft),
        _ => None,
    }
}

fn motion_for_key(character: char) -> Option<Motion> {
    match character {
        'h' => Some(Motion::Left),
        'j' => Some(Motion::Down),
        'k' => Some(Motion::Up),
        'l' => Some(Motion::Right),
        '0' => Some(Motion::LineStart),
        '^' => Some(Motion::FirstNonBlank),
        '$' => Some(Motion::LineEnd),
        'w' => Some(Motion::WordForward),
        'W' => Some(Motion::WordForwardBig),
        'b' => Some(Motion::WordBackward),
        'B' => Some(Motion::WordBackwardBig),
        'e' => Some(Motion::WordEnd),
        'E' => Some(Motion::WordEndBig),
        '(' => Some(Motion::SentenceBackward),
        ')' => Some(Motion::SentenceForward),
        '{' => Some(Motion::ParagraphBackward),
        '}' => Some(Motion::ParagraphForward),
        '%' => Some(Motion::MatchingPair),
        _ => None,
    }
}

fn find_kind_for_key(character: char) -> Option<FindKind> {
    match character {
        'f' => Some(FindKind::Forward),
        'F' => Some(FindKind::Backward),
        't' => Some(FindKind::TillForward),
        'T' => Some(FindKind::TillBackward),
        _ => None,
    }
}

fn is_till_find_kind(kind: FindKind) -> bool {
    matches!(kind, FindKind::TillForward | FindKind::TillBackward)
}

fn reverse_find_kind(kind: FindKind) -> FindKind {
    match kind {
        FindKind::Forward => FindKind::Backward,
        FindKind::Backward => FindKind::Forward,
        FindKind::TillForward => FindKind::TillBackward,
        FindKind::TillBackward => FindKind::TillForward,
    }
}

fn find_operator_range(
    document: &Document,
    cursor: CharPos,
    target: CharPos,
    kind: FindKind,
) -> CharRange {
    match kind {
        FindKind::Forward | FindKind::TillForward => {
            CharRange::new(cursor, target.advance(1).clamp(document.len_chars()))
        }
        FindKind::Backward | FindKind::TillBackward => CharRange::new(target, cursor),
    }
}

fn register_kind(kind: RangeKind) -> RegisterKind {
    match kind {
        RangeKind::Characterwise => RegisterKind::Characterwise,
        RangeKind::Linewise => RegisterKind::Linewise,
        RangeKind::Blockwise => RegisterKind::Blockwise,
    }
}

/// Converts byte ranges in `text` to character ranges.
fn to_char_ranges(text: &str, byte_ranges: &[std::ops::Range<usize>]) -> Vec<CharRange> {
    // A single walk, recording the character index whenever a byte offset of
    // interest is passed.
    let mut wanted: Vec<Option<usize>> = vec![None; byte_ranges.len()];
    let mut index = 0usize;
    let mut next = 0usize;
    for (offset, _) in text.char_indices() {
        while next < byte_ranges.len() && byte_ranges[next].start <= offset {
            wanted[next] = Some(index);
            next += 1;
        }
        index += 1;
        // Ranges are ascending, so once every start has been passed there is
        // nothing left to record.
        if next == byte_ranges.len() {
            break;
        }
    }
    // A range that starts at the very end of the text has no character to land
    // on, so the walk never sees it. Ranges are ascending and a matcher cannot
    // report a start past the end, so anything still unassigned starts exactly
    // there -- which is what `$` matches at the end of the last line.
    let total = index;
    while next < byte_ranges.len() {
        wanted[next] = Some(total);
        next += 1;
    }
    byte_ranges
        .iter()
        .enumerate()
        .filter_map(|(position, range)| {
            let start = wanted[position]?;
            let end = start + text[range.start..range.end].chars().count();
            Some(CharRange::new(CharPos(start), CharPos(end)))
        })
        .collect()
}

/// Characters pulled from the rope per chunk while searching. Large enough that
/// the substring search does the matching, small enough to stay cache resident.
const SEARCH_CHUNK: usize = 1 << 16;

fn find_search(
    document: &Document,
    cursor: CharPos,
    query: &str,
    backwards: bool,
    buffer: &mut String,
) -> Option<CharPos> {
    let needle = query.chars().count();
    let len = document.len_chars();
    if needle == 0 || needle > len {
        return None;
    }
    if backwards {
        if cursor.0 == 0 {
            return None;
        }
        let limit = cursor.0.min(len);
        find_last_match(document, query, limit, buffer)
    } else {
        let from = cursor.0.saturating_add(1);
        if from + needle > len {
            return None;
        }
        find_first_match(document, query, from, buffer)
    }
}

/// Start of the first occurrence of `query` at or after `from`.
///
/// The document is examined one overlapping chunk at a time and matched with the
/// standard library's substring search, so the cost is driven by the document
/// size rather than by one rope lookup per candidate position.
///
/// The scan starts at the chunk holding `from`, not at the start of the document.
/// Every chunk before it would be read only to be discarded, since a match there
/// starts before `from` and is rejected anyway; skipping them makes a search from
/// deep in the document cost what a search from the top costs, rather than adding
/// a pass over the prefix. Matches that begin before `from` and reach into this
/// chunk are still rejected, by the `earliest` skip below.
fn find_first_match(
    document: &Document,
    query: &str,
    from: usize,
    buffer: &mut String,
) -> Option<CharPos> {
    let len = document.len_chars();
    let needle = query.chars().count();
    buffer.clear();
    buffer.reserve(SEARCH_CHUNK + needle);
    let mut chunk = (from / SEARCH_CHUNK) * SEARCH_CHUNK;
    while chunk < len {
        // Overlap by the needle so matches spanning a boundary stay visible.
        let end = (chunk + SEARCH_CHUNK + needle - 1).min(len);
        buffer.clear();
        document.write_slice(CharRange::new(CharPos(chunk), CharPos(end)), &mut *buffer);
        // Only the first chunk can begin before `from`, so only it pays the skip.
        let start_byte = if from > chunk {
            let skip = from - chunk;
            buffer.chars().take(skip).map(char::len_utf8).sum()
        } else {
            0
        };
        if let Some(found) = buffer[start_byte..].find(query) {
            let at = start_byte + found;
            return Some(CharPos(chunk + buffer[..at].chars().count()));
        }
        chunk += SEARCH_CHUNK;
    }
    None
}

/// Start of the last occurrence of `query` strictly before `limit`.
///
/// A match may begin before `limit` and still extend past it, so candidates are
/// rejected on their start position rather than on where the needle ends.
fn find_last_match(
    document: &Document,
    query: &str,
    limit: usize,
    buffer: &mut String,
) -> Option<CharPos> {
    let len = document.len_chars();
    let needle = query.chars().count();
    buffer.clear();
    buffer.reserve(SEARCH_CHUNK + needle);
    let mut chunk = (limit - 1) / SEARCH_CHUNK * SEARCH_CHUNK;
    loop {
        let end = (chunk + SEARCH_CHUNK + needle - 1).min(len);
        buffer.clear();
        document.write_slice(CharRange::new(CharPos(chunk), CharPos(end)), &mut *buffer);
        let mut searchable = buffer.len();
        while searchable > 0 {
            let Some(found) = buffer[..searchable].rfind(query) else {
                break;
            };
            let start = chunk + buffer[..found].chars().count();
            if start < limit {
                return Some(CharPos(start));
            }
            // Keep the matches that begin earlier.
            searchable = found;
        }
        if chunk == 0 {
            return None;
        }
        chunk -= SEARCH_CHUNK;
    }
}

fn find_target(
    document: &Document,
    cursor: CharPos,
    kind: FindKind,
    character: char,
    count: usize,
    skip_adjacent: bool,
) -> Option<CharPos> {
    let line = document.pos_to_line_col(cursor).line;
    let line_start = document.line_start(line);
    let line_end = document.line_end(line);
    let cursor = CharPos(cursor.0.clamp(line_start.0, line_end.0));
    let count = count.max(1);
    match kind {
        FindKind::Forward | FindKind::TillForward => {
            let mut index = cursor.0.saturating_add(1);
            if skip_adjacent {
                index = index.saturating_add(1);
            }
            let mut found = 0;
            for (position, candidate) in document
                .chars_from(CharPos(index))
                .take_while(|(p, _)| p.0 < line_end.0)
            {
                if candidate == character {
                    found += 1;
                    if found == count {
                        return Some(if is_till_find_kind(kind) {
                            CharPos(position.0 - 1)
                        } else {
                            position
                        });
                    }
                }
            }
            None
        }
        FindKind::Backward | FindKind::TillBackward => {
            if cursor.0 <= line_start.0 {
                return None;
            }
            let mut index = cursor.0 - 1;
            if skip_adjacent && index > line_start.0 {
                index -= 1;
            }
            let mut found = 0;
            for (at, candidate) in document.chars_before(CharPos(index + 1)) {
                if at.0 < line_start.0 {
                    break;
                }
                if candidate == character {
                    found += 1;
                    if found == count {
                        if !is_till_find_kind(kind) {
                            return Some(CharPos(at.0));
                        }
                        let landing = at.0 + 1;
                        if landing >= line_end.0 {
                            return None;
                        }
                        return Some(CharPos(landing));
                    }
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{to_char_ranges, Editor, Key, Mode, Operator, Substitution};
    use crate::core::position::CharPos;

    fn keys(editor: &mut Editor, keys: &[Key]) {
        for key in keys {
            editor.handle_key(*key);
        }
    }

    #[test]
    fn supports_insert_and_normal_modes() {
        let mut editor = Editor::new();
        keys(
            &mut editor,
            &[Key::Char('i'), Key::Char('h'), Key::Char('i'), Key::Escape],
        );
        assert_eq!(editor.text(), "hi");
        assert_eq!(editor.mode, Mode::Normal);
    }

    #[test]
    fn inserts_a_real_tab_character() {
        let mut editor = Editor::new();
        keys(&mut editor, &[Key::Char('i'), Key::Tab, Key::Escape]);
        assert_eq!(editor.text(), "\t");
    }

    #[test]
    fn supports_append_at_line_end() {
        let mut editor = Editor::from_text("one\ntwo");
        editor.cursor = CharPos(1);
        keys(&mut editor, &[Key::Char('A'), Key::Char('!'), Key::Escape]);
        assert_eq!(editor.text(), "one!\ntwo");
        assert_eq!(editor.mode, Mode::Normal);

        let mut editor = Editor::from_text("one\n");
        editor.cursor = CharPos(2);
        keys(&mut editor, &[Key::Char('a'), Key::Char('!'), Key::Escape]);
        assert_eq!(editor.text(), "one!\n");
    }

    #[test]
    fn escape_from_insert_moves_to_previous_character() {
        let mut editor = Editor::from_text("abc");
        editor.cursor = CharPos(0);
        keys(&mut editor, &[Key::Char('i'), Key::Char('X'), Key::Escape]);
        assert_eq!(editor.cursor, CharPos(0));
        assert_eq!(editor.text(), "Xabc");

        let mut editor = Editor::from_text("abc");
        editor.cursor = CharPos(0);
        keys(&mut editor, &[Key::Char('a'), Key::Char('X'), Key::Escape]);
        assert_eq!(editor.cursor, CharPos(1));
        assert_eq!(editor.text(), "aXbc");

        let mut editor = Editor::from_text("abc");
        keys(&mut editor, &[Key::Char('A'), Key::Char('X'), Key::Escape]);
        assert_eq!(editor.cursor, CharPos(3));
        assert_eq!(editor.text(), "abcX");
    }

    #[test]
    fn supports_counts_and_delete_motion() {
        let mut editor = Editor::from_text("one two three");
        keys(
            &mut editor,
            &[Key::Char('2'), Key::Char('d'), Key::Char('w')],
        );
        assert_eq!(editor.text(), "three");
    }

    #[test]
    fn supports_text_object_operator() {
        let mut editor = Editor::from_text("call foo(a, b)");
        editor.cursor = CharPos(9);
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('i'), Key::Char('(')],
        );
        assert_eq!(editor.text(), "call foo()");
    }

    #[test]
    fn supports_visual_and_change_word_objects() {
        let mut editor = Editor::from_text("one two");
        editor.cursor = CharPos(1);
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('i'), Key::Char('w')],
        );
        let selection = editor.selection.as_ref().unwrap();
        assert_eq!(
            editor.document.slice(selection.ranges(&editor.document)[0]),
            "one"
        );
        assert_eq!(
            editor.mode,
            Mode::Visual(crate::core::SelectionKind::Characterwise)
        );

        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('a'), Key::Char('w')],
        );
        let selection = editor.selection.as_ref().unwrap();
        assert_eq!(
            editor.document.slice(selection.ranges(&editor.document)[0]),
            "one "
        );

        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('c'), Key::Char('i'), Key::Char('w')],
        );
        assert_eq!(editor.text(), " two");
        assert_eq!(editor.mode, Mode::Insert);

        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('c'), Key::Char('a'), Key::Char('w')],
        );
        assert_eq!(editor.text(), "two");
        assert_eq!(editor.mode, Mode::Insert);

        let mut editor = Editor::from_text("one two");
        editor.cursor = CharPos(1);
        keys(
            &mut editor,
            &[
                Key::Char('v'),
                Key::Char('i'),
                Key::Char('w'),
                Key::Char('d'),
            ],
        );
        assert_eq!(editor.text(), " two");

        let mut editor = Editor::from_text("one two");
        editor.cursor = CharPos(1);
        keys(
            &mut editor,
            &[
                Key::Char('v'),
                Key::Char('a'),
                Key::Char('w'),
                Key::Char('d'),
            ],
        );
        assert_eq!(editor.text(), "two");
    }

    #[test]
    fn supports_standard_text_object_command_forms() {
        let mut editor = Editor::from_text("call foo(a, b)");
        editor.cursor = CharPos(10);
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('i'), Key::Char('(')],
        );
        assert_eq!(editor.text(), "call foo()");

        let mut editor = Editor::from_text("one\ntwo\n\nthree");
        editor.cursor = CharPos(1);
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('a'), Key::Char('p')],
        );
        assert_eq!(
            editor.mode,
            Mode::Visual(crate::core::SelectionKind::Linewise)
        );
        keys(&mut editor, &[Key::Char('d')]);
        assert_eq!(editor.text(), "three");
    }

    #[test]
    fn supports_change_and_yank_flow() {
        let mut editor = Editor::from_text("alpha beta");
        editor.cursor = CharPos(0);
        keys(
            &mut editor,
            &[
                Key::Char('y'),
                Key::Char('i'),
                Key::Char('w'),
                Key::Char('p'),
            ],
        );
        assert_eq!(editor.text(), "alpha alpha beta");

        let mut editor = Editor::from_text("alpha beta");
        keys(
            &mut editor,
            &[
                Key::Char('c'),
                Key::Char('i'),
                Key::Char('w'),
                Key::Char('x'),
                Key::Escape,
            ],
        );
        assert_eq!(editor.text(), "x beta");
        assert_eq!(editor.mode, Mode::Normal);
    }

    #[test]
    fn supports_linewise_double_operator() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        keys(&mut editor, &[Key::Char('d'), Key::Char('d')]);
        assert_eq!(editor.text(), "two\nthree");
    }

    #[test]
    fn supports_cgg_and_cg_linewise_operators() {
        let mut editor = Editor::from_text("zero\none\ntwo\nthree");
        editor.cursor = CharPos(10);
        keys(
            &mut editor,
            &[Key::Char('c'), Key::Char('g'), Key::Char('g'), Key::Escape],
        );
        assert_eq!(editor.text(), "three");

        let mut editor = Editor::from_text("zero\none\ntwo\nthree");
        editor.cursor = CharPos::ZERO;
        keys(&mut editor, &[Key::Char('c'), Key::Char('G'), Key::Escape]);
        assert_eq!(editor.text(), "");

        let mut editor = Editor::from_text("zero\none\ntwo\nthree");
        editor.cursor = CharPos(10);
        keys(&mut editor, &[Key::Char('c'), Key::Char('G'), Key::Escape]);
        assert_eq!(editor.text(), "zero\none\n");
    }

    #[test]
    fn supports_percent_motion_and_operators() {
        let mut editor = Editor::from_text("foo(bar)");
        editor.cursor = CharPos(3);
        keys(&mut editor, &[Key::Char('%')]);
        assert_eq!(editor.cursor, CharPos(7));
        keys(&mut editor, &[Key::Char('%')]);
        assert_eq!(editor.cursor, CharPos(3));

        let mut editor = Editor::from_text("foo(bar)");
        keys(&mut editor, &[Key::Char('%')]);
        assert_eq!(editor.cursor, CharPos(7));

        let mut editor = Editor::from_text("foo(bar)");
        editor.cursor = CharPos(3);
        keys(&mut editor, &[Key::Char('d'), Key::Char('%')]);
        assert_eq!(editor.text(), "foo");

        let mut editor = Editor::from_text("foo(bar)");
        editor.cursor = CharPos(3);
        keys(&mut editor, &[Key::Char('c'), Key::Char('%')]);
        assert_eq!(editor.text(), "foo");
        assert_eq!(editor.mode, Mode::Insert);

        let mut editor = Editor::from_text("foo(bar)");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('%'), Key::Char('d')],
        );
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn supports_forward_and_backward_character_find() {
        let mut editor = Editor::from_text("ba");
        keys(&mut editor, &[Key::Char('f'), Key::Char('a')]);
        assert_eq!(editor.cursor, CharPos(1));
        assert_eq!(editor.mode, Mode::Normal);

        let mut editor = Editor::from_text("a-b-c");
        keys(
            &mut editor,
            &[Key::Char('2'), Key::Char('f'), Key::Char('-')],
        );
        assert_eq!(editor.cursor, CharPos(3));

        let mut editor = Editor::from_text("a-b-c");
        editor.cursor = CharPos(4);
        keys(
            &mut editor,
            &[Key::Char('2'), Key::Char('F'), Key::Char('-')],
        );
        assert_eq!(editor.cursor, CharPos(1));

        let mut editor = Editor::from_text("a1a");
        keys(&mut editor, &[Key::Char('f'), Key::Char('1')]);
        assert_eq!(editor.cursor, CharPos(1));

        let mut editor = Editor::from_text("a f f");
        keys(&mut editor, &[Key::Char('f'), Key::Char('f')]);
        assert_eq!(editor.cursor, CharPos(2));
    }

    #[test]
    fn character_find_does_not_cross_lines() {
        let mut editor = Editor::from_text("ab\nx");
        keys(&mut editor, &[Key::Char('f'), Key::Char('x')]);
        assert_eq!(editor.cursor, CharPos(0));
        assert_eq!(editor.message, "Character not found");

        let mut editor = Editor::from_text("x\nab");
        editor.cursor = CharPos(3);
        keys(&mut editor, &[Key::Char('F'), Key::Char('x')]);
        assert_eq!(editor.cursor, CharPos(3));
        assert_eq!(editor.message, "Character not found");
    }

    #[test]
    fn repeats_character_find_with_direction_and_count() {
        let mut editor = Editor::from_text("a-a-a-a");
        keys(&mut editor, &[Key::Char('f'), Key::Char('a')]);
        assert_eq!(editor.cursor, CharPos(2));
        keys(&mut editor, &[Key::Char('2'), Key::Char(';')]);
        assert_eq!(editor.cursor, CharPos(6));
        keys(&mut editor, &[Key::Char(',')]);
        assert_eq!(editor.cursor, CharPos(4));
        keys(&mut editor, &[Key::Char(';')]);
        assert_eq!(editor.cursor, CharPos(6));
        keys(&mut editor, &[Key::Char(',')]);
        assert_eq!(editor.cursor, CharPos(4));

        let mut editor = Editor::from_text("abcabcabcabc");
        keys(&mut editor, &[Key::Char('f'), Key::Char('a')]);
        keys(&mut editor, &[Key::Char('2'), Key::Char(';')]);
        keys(&mut editor, &[Key::Char('l')]);
        assert_eq!(editor.cursor, CharPos(10));

        let mut editor = Editor::from_text("aaba");
        keys(&mut editor, &[Key::Char('t'), Key::Char('a')]);
        assert_eq!(editor.cursor, CharPos(0));
        keys(&mut editor, &[Key::Char(';')]);
        assert_eq!(editor.cursor, CharPos(2));
    }

    #[test]
    fn supports_character_find_operators() {
        let mut editor = Editor::from_text("abca");
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('f'), Key::Char('a')],
        );
        assert_eq!(editor.text(), "");

        let mut editor = Editor::from_text("abca");
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('t'), Key::Char('a')],
        );
        assert_eq!(editor.text(), "a");

        let mut editor = Editor::from_text("abca");
        editor.cursor = CharPos(3);
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('F'), Key::Char('a')],
        );
        assert_eq!(editor.text(), "a");

        let mut editor = Editor::from_text("abca");
        editor.cursor = CharPos(3);
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('T'), Key::Char('a')],
        );
        assert_eq!(editor.text(), "aa");

        let mut editor = Editor::from_text("abcde");
        editor.cursor = CharPos(4);
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('F'), Key::Char('b')],
        );
        assert_eq!(editor.text(), "ae");

        let mut editor = Editor::from_text("abcde");
        editor.cursor = CharPos(4);
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('T'), Key::Char('b')],
        );
        assert_eq!(editor.text(), "abe");

        let mut editor = Editor::from_text("a--c");
        editor.cursor = CharPos(3);
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('T'), Key::Char('-')],
        );
        assert_eq!(editor.text(), "a--c");

        let mut editor = Editor::from_text("a--c");
        editor.cursor = CharPos(3);
        keys(
            &mut editor,
            &[Key::Char('c'), Key::Char('T'), Key::Char('-')],
        );
        assert_eq!(editor.text(), "a--c");
        assert_eq!(editor.mode, Mode::Insert);

        let mut editor = Editor::from_text("abc");
        editor.cursor = CharPos(3);
        keys(&mut editor, &[Key::Char('T'), Key::Char('c')]);
        assert_eq!(editor.cursor, CharPos(3));
        assert_eq!(editor.message, "Character not found");

        let mut editor = Editor::from_text("abababa");
        keys(&mut editor, &[Key::Char('f'), Key::Char('a')]);
        keys(&mut editor, &[Key::Char('d'), Key::Char(';')]);
        assert_eq!(editor.text(), "abba");

        let mut editor = Editor::from_text("abacada");
        editor.cursor = CharPos(6);
        keys(
            &mut editor,
            &[
                Key::Char('d'),
                Key::Char('2'),
                Key::Char('F'),
                Key::Char('a'),
            ],
        );
        assert_eq!(editor.text(), "aba");

        let mut editor = Editor::from_text("abc");
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('f'), Key::Char('z')],
        );
        assert_eq!(editor.text(), "abc");
        assert_eq!(editor.mode, Mode::Normal);
        assert_eq!(editor.message, "Character not found");
    }

    #[test]
    fn supports_character_find_in_visual_mode() {
        let mut editor = Editor::from_text("a-b-c-d");
        keys(
            &mut editor,
            &[
                Key::Char('v'),
                Key::Char('2'),
                Key::Char('f'),
                Key::Char('-'),
            ],
        );
        let selection = editor.selection.as_ref().unwrap();
        assert_eq!(
            editor.document.slice(selection.ranges(&editor.document)[0]),
            "a-b-"
        );
        keys(&mut editor, &[Key::Char(';')]);
        let selection = editor.selection.as_ref().unwrap();
        assert_eq!(
            editor.document.slice(selection.ranges(&editor.document)[0]),
            "a-b-c-"
        );

        let mut editor = Editor::from_text("ba");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('f'), Key::Char('a')],
        );
        let selection = editor.selection.as_ref().unwrap();
        assert_eq!(
            editor.document.slice(selection.ranges(&editor.document)[0]),
            "ba"
        );
    }

    #[test]
    fn visual_character_find_updates_block_boundaries() {
        let mut editor = Editor::from_text("abcdefghij");
        editor.cursor = CharPos(3);
        keys(
            &mut editor,
            &[
                Key::Ctrl('v'),
                Key::Char('f'),
                Key::Char('f'),
                Key::Char('F'),
                Key::Char('e'),
            ],
        );
        let selection = editor.selection.as_ref().unwrap();
        assert_eq!(
            editor.document.slice(selection.ranges(&editor.document)[0]),
            "de"
        );
    }

    #[test]
    fn visual_count_does_not_leak_into_normal_mode() {
        let mut editor = Editor::from_text("a0z0z");
        keys(&mut editor, &[Key::Char('v'), Key::Char('2'), Key::Escape]);
        keys(&mut editor, &[Key::Char('f'), Key::Char('z')]);
        assert_eq!(editor.cursor, CharPos(2));

        let mut editor = Editor::from_text("abc\ndef\nghi");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('2'), Key::Char('d')],
        );
        keys(&mut editor, &[Key::Char('G')]);
        assert_eq!(editor.cursor, CharPos(7));
    }

    #[test]
    fn visual_goto_last_extends_selection_to_end_of_file() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        keys(&mut editor, &[Key::Char('v'), Key::Char('G')]);
        assert_eq!(
            editor.mode,
            Mode::Visual(crate::core::SelectionKind::Characterwise)
        );
        assert_eq!(editor.cursor, CharPos(8));
        let selection = editor.selection.clone().unwrap();
        assert_eq!(selection.active, CharPos(8));
        assert_eq!(
            editor.document.slice(selection.ranges(&editor.document)[0]),
            "one\ntwo\nt"
        );

        keys(&mut editor, &[Key::Char('y')]);
        assert_eq!(editor.take_clipboard_text().as_deref(), Some("one\ntwo\nt"));
    }

    #[test]
    fn visual_goto_last_with_count_jumps_to_that_line() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        editor.cursor = CharPos(4);
        keys(
            &mut editor,
            &[Key::Char('V'), Key::Char('3'), Key::Char('G')],
        );
        assert_eq!(editor.cursor, CharPos(8));
        keys(&mut editor, &[Key::Char('y')]);
        assert_eq!(editor.take_clipboard_text().as_deref(), Some("two\nthree"));
    }

    #[test]
    fn visual_motion_syncs_cursor_with_selection_active() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('j'), Key::Char('j')],
        );
        assert_eq!(editor.cursor, CharPos(8));
        keys(&mut editor, &[Key::Char('o')]);
        assert_eq!(editor.cursor, CharPos(0));
        assert_eq!(editor.selection.as_ref().unwrap().active, CharPos(0));
    }

    #[test]
    fn visual_text_object_syncs_cursor_with_selection_active() {
        let mut editor = Editor::from_text("alpha beta");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('i'), Key::Char('w')],
        );
        assert_eq!(editor.cursor, CharPos(4));
    }

    #[test]
    fn visual_yank_updates_register_and_queues_clipboard_text() {
        let mut editor = Editor::from_text("é界");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('l'), Key::Char('y')],
        );
        assert_eq!(editor.take_clipboard_text().as_deref(), Some("é界"));
        keys(&mut editor, &[Key::Char('p')]);
        assert_eq!(editor.text(), "é界é界");
    }

    #[test]
    fn visual_line_yank_queues_linewise_clipboard_text() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        keys(&mut editor, &[Key::Char('V'), Key::Char('y')]);
        assert_eq!(editor.take_clipboard_text().as_deref(), Some("one\n"));
    }

    #[test]
    fn visual_block_yank_queues_selected_lines() {
        let mut editor = Editor::from_text("abc\ndef");
        keys(
            &mut editor,
            &[Key::Ctrl('v'), Key::Char('l'), Key::Down, Key::Char('y')],
        );
        assert_eq!(editor.take_clipboard_text().as_deref(), Some("ab\nde"));
    }

    #[test]
    fn supports_visual_character_delete() {
        let mut editor = Editor::from_text("abcd");
        keys(
            &mut editor,
            &[
                Key::Char('v'),
                Key::Char('l'),
                Key::Char('l'),
                Key::Char('d'),
            ],
        );
        assert_eq!(editor.text(), "d");
    }

    #[test]
    fn supports_undo_and_redo() {
        let mut editor = Editor::from_text("a");
        keys(&mut editor, &[Key::Char('x')]);
        assert_eq!(editor.text(), "");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "a");
        keys(&mut editor, &[Key::Ctrl('r')]);
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn supports_linewise_and_block_visual_delete() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        keys(&mut editor, &[Key::Char('V'), Key::Char('d')]);
        assert_eq!(editor.text(), "two\nthree");

        let mut editor = Editor::from_text("abc\ndef");
        keys(
            &mut editor,
            &[Key::Ctrl('v'), Key::Char('l'), Key::Down, Key::Char('d')],
        );
        assert_eq!(editor.text(), "c\nf");
    }

    #[test]
    fn supports_end_of_line_operator() {
        let mut editor = Editor::from_text("hello world");
        editor.cursor = CharPos(6);
        keys(&mut editor, &[Key::Char('d'), Key::Char('$')]);
        assert_eq!(editor.text(), "hello ");
    }

    #[test]
    fn groups_insert_mode_into_one_undo() {
        let mut editor = Editor::new();
        keys(
            &mut editor,
            &[Key::Char('i'), Key::Char('a'), Key::Char('b'), Key::Escape],
        );
        assert_eq!(editor.text(), "ab");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn supports_search_and_repeat() {
        let mut editor = Editor::from_text("one two one one");
        editor.cursor = CharPos(1);
        keys(
            &mut editor,
            &[
                Key::Char('/'),
                Key::Char('o'),
                Key::Char('n'),
                Key::Char('e'),
                Key::Enter,
            ],
        );
        assert_eq!(editor.cursor, CharPos(8));
        keys(&mut editor, &[Key::Char('n')]);
        assert_eq!(editor.cursor, CharPos(12));
        keys(&mut editor, &[Key::Char('N')]);
        assert_eq!(editor.cursor, CharPos(8));
    }

    /// Selects `count` characters on one line, starting at the cursor.
    fn select_characters(editor: &mut Editor, count: usize) {
        keys(&mut *editor, &[Key::Char('v')]);
        for _ in 1..count {
            keys(&mut *editor, &[Key::Char('l')]);
        }
    }

    #[test]
    fn visual_block_delete_undoes_in_one_step() {
        let mut editor = Editor::from_text("aaa\nbbb\nccc\nddd");
        editor.cursor = CharPos(1);
        keys(
            &mut editor,
            &[Key::Ctrl('v'), Key::Char('j'), Key::Char('j')],
        );
        keys(&mut editor, &[Key::Char('d')]);
        // Column 1 is dropped from rows 0..=2.
        assert_eq!(editor.text(), "aa\nbb\ncc\nddd");
        // One `u` has to bring back all three rows, not just the last one.
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "aaa\nbbb\nccc\nddd");
    }

    #[test]
    fn visual_character_delete_over_several_columns_undoes_in_one_step() {
        let mut editor = Editor::from_text("abcdef");
        select_characters(&mut editor, 4);
        keys(&mut editor, &[Key::Char('d')]);
        assert_eq!(editor.text(), "ef");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "abcdef");
    }

    #[test]
    fn visual_block_change_undoes_the_deletion_and_the_typed_text_together() {
        let mut editor = Editor::from_text("aaa\nbbb\nccc");
        editor.cursor = CharPos::ZERO;
        keys(&mut editor, &[Key::Ctrl('v'), Key::Char('j')]);
        keys(&mut editor, &[Key::Char('c')]);
        assert_eq!(editor.text(), "aa\nbb\nccc");
        keys(&mut editor, &[Key::Char('x'), Key::Escape]);
        assert_eq!(editor.text(), "xaa\nbb\nccc");
        // The change and everything typed into it are one transaction.
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "aaa\nbbb\nccc");
    }

    #[test]
    fn a_visual_delete_does_not_absorb_the_next_unrelated_edit() {
        let mut editor = Editor::from_text("abcd");
        select_characters(&mut editor, 2);
        keys(&mut editor, &[Key::Char('d')]);
        assert_eq!(editor.text(), "cd");
        keys(&mut editor, &[Key::Char('x')]);
        assert_eq!(editor.text(), "d");
        // Two separate user actions, so two separate undos.
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "cd");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "abcd");
    }

    #[test]
    fn normal_mode_yank_reaches_the_system_clipboard() {
        // A linewise yank carries the line break when the line has one.
        let mut editor = Editor::from_text("alpha beta\ngamma");
        keys(&mut editor, &[Key::Char('y'), Key::Char('y')]);
        assert_eq!(
            editor.take_clipboard_text().as_deref(),
            Some("alpha beta\n")
        );

        // The final line of a file with no trailing newline has none to carry.
        let mut editor = Editor::from_text("alpha beta");
        keys(&mut editor, &[Key::Char('y'), Key::Char('y')]);
        assert_eq!(editor.take_clipboard_text().as_deref(), Some("alpha beta"));

        // Characterwise yanks go through the same path.
        let mut editor = Editor::from_text("one two");
        editor.cursor = CharPos(1);
        keys(
            &mut editor,
            &[Key::Char('y'), Key::Char('i'), Key::Char('w')],
        );
        assert_eq!(editor.take_clipboard_text().as_deref(), Some("one"));
    }

    #[test]
    fn substitution_replaces_every_occurrence_and_counts_them() {
        let mut editor = Editor::from_text("one two one one");
        assert_eq!(editor.replace_literal("one", "1", true), 3);
        assert_eq!(editor.text(), "1 two 1 1");

        // Without `g` only the first is replaced.
        let mut editor = Editor::from_text("one two one");
        assert_eq!(editor.replace_literal("one", "1", false), 1);
        assert_eq!(editor.text(), "1 two one");
    }

    #[test]
    fn substitution_handles_replacements_that_change_the_length() {
        // Growing: every `a` has to be found at a position that keeps shifting.
        let mut editor = Editor::from_text("aaa");
        assert_eq!(editor.replace_literal("a", "bb", true), 3);
        assert_eq!(editor.text(), "bbbbbb");

        // Shrinking to nothing.
        let mut editor = Editor::from_text("aXaXa");
        assert_eq!(editor.replace_literal("a", "", true), 3);
        assert_eq!(editor.text(), "XX");

        // Growing across a chunk boundary in a long line.
        let long = "x".repeat(70_000);
        let mut editor = Editor::from_text(&long);
        let count = editor.replace_literal("x", "yy", false);
        assert_eq!(count, 1);
        assert_eq!(editor.document.len_chars(), 70_001);
    }

    #[test]
    fn substitution_matches_without_overlapping() {
        // `str::match_indices` advances past each match rather than rescanning
        // from one character in, so "aaaa" has two "aa" matches, not three.
        let mut editor = Editor::from_text("aaaa");
        assert_eq!(editor.replace_literal("aa", "b", true), 2);
        assert_eq!(editor.text(), "bb");
    }

    #[test]
    fn substitution_is_one_undo_step_and_one_redo_step() {
        let mut editor = Editor::from_text("aaXaaXaa");
        assert_eq!(editor.replace_literal("aa", "zz", true), 3);
        assert_eq!(editor.text(), "zzXzzXzz");

        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "aaXaaXaa", "one undo rewinds every match");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "aaXaaXaa", "there is nothing further back");

        keys(&mut editor, &[Key::Ctrl('r')]);
        assert_eq!(editor.text(), "zzXzzXzz");
    }

    #[test]
    fn substitution_undoes_correctly_when_the_length_changed() {
        // The undo record holds positions from the document as it stood when each
        // match was replaced, so a growth or a shrink has to be accounted for on
        // the way back.
        let mut editor = Editor::from_text("a-b-a-b-a");
        assert_eq!(editor.replace_literal("a", "LONGER", true), 3);
        assert_eq!(editor.text(), "LONGER-b-LONGER-b-LONGER");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "a-b-a-b-a");

        let mut editor = Editor::from_text("LONGER-b-LONGER-b-LONGER");
        editor.replace_literal("LONGER", "a", true);
        assert_eq!(editor.text(), "a-b-a-b-a");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "LONGER-b-LONGER-b-LONGER");
    }

    #[test]
    fn substitution_handles_unicode_positions() {
        let mut editor = Editor::from_text("世界世界");
        assert_eq!(editor.replace_literal("世界", "ab", true), 2);
        assert_eq!(editor.text(), "abab");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "世界世界");
    }

    #[test]
    fn substitution_reports_a_pattern_that_is_absent() {
        let mut editor = Editor::from_text("one two");
        editor.mark_saved();
        assert_eq!(editor.replace_literal("zzz", "x", true), 0);
        assert_eq!(editor.text(), "one two");
        assert!(!editor.is_dirty());
        assert_eq!(editor.message, "Pattern not found: zzz");
    }

    #[test]
    fn substitution_that_changes_nothing_leaves_the_buffer_clean() {
        let mut editor = Editor::from_text("one two one");
        editor.mark_saved();
        assert_eq!(editor.replace_literal("one", "one", true), 2);
        assert_eq!(editor.text(), "one two one");
        assert!(
            !editor.is_dirty(),
            "swapping text for itself must not rewrite the rope"
        );
        // And nothing was recorded, so undo has nothing to rewind.
        assert_eq!(editor.replace_literal("one", "1", true), 2);
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "one two one");
    }

    #[test]
    fn substitution_does_not_rescan_the_text_it_just_wrote() {
        // The search position has to land past the replacement, not past the
        // needle. When the replacement contains the needle, resuming from the
        // needle's length would find the replacement's own first character and
        // keep growing the document without end.
        let mut editor = Editor::from_text("aa");
        assert_eq!(editor.replace_literal("a", "aa", true), 2);
        assert_eq!(editor.text(), "aaaa", "two matches, not an endless loop");

        let mut editor = Editor::from_text("aaaa");
        assert_eq!(editor.replace_literal("aa", "a", true), 2);
        assert_eq!(editor.text(), "aa");
    }

    #[test]
    fn deleting_fills_the_register_so_the_text_can_be_put_back() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        keys(&mut editor, &[Key::Char('d'), Key::Char('d')]);
        assert_eq!(editor.text(), "two\nthree");
        keys(&mut editor, &[Key::Char('p')]);
        // `p` goes after the current line, which is now "two".
        assert_eq!(editor.text(), "two\none\nthree");

        // One undo rewinds the delete; a second is not needed to get back.
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "two\nthree");
    }

    #[test]
    fn changing_fills_the_register_too() {
        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('d'), Key::Char('i'), Key::Char('w')],
        );
        assert_eq!(editor.text(), " two");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "one two");
        // Vim separates a characterwise paste from the word it follows.
        keys(&mut editor, &[Key::Char('p')]);
        assert_eq!(editor.text(), "one one two");
    }

    #[test]
    fn the_black_hole_discards_a_delete() {
        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('y'), Key::Char('i'), Key::Char('w')],
        );
        // `"_d i w` deletes the word without disturbing what was yanked.
        keys(
            &mut editor,
            &[
                Key::Char('"'),
                Key::Char('_'),
                Key::Char('d'),
                Key::Char('i'),
                Key::Char('w'),
            ],
        );
        assert_eq!(editor.text(), " two");
        keys(&mut editor, &[Key::Char('p')]);
        assert_eq!(editor.text(), " onetwo", "the earlier yank survived");
    }

    #[test]
    fn the_black_hole_also_discards_a_yank() {
        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('y'), Key::Char('i'), Key::Char('w')],
        );
        keys(
            &mut editor,
            &[
                Key::Char('"'),
                Key::Char('_'),
                Key::Char('y'),
                Key::Char('i'),
                Key::Char('w'),
            ],
        );
        keys(&mut editor, &[Key::Char('p')]);
        assert_eq!(editor.text(), "one one two");
    }

    #[test]
    fn pasting_from_the_black_hole_inserts_nothing() {
        let mut editor = Editor::from_text("one");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('_'), Key::Char('p')],
        );
        assert_eq!(editor.text(), "one");
    }

    #[test]
    fn an_unknown_register_aborts_the_command() {
        let mut editor = Editor::from_text("one\ntwo");
        // `"a` names a register this version does not have, so the `d` after it
        // must not run against the unnamed register.
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('a'), Key::Char('d')],
        );
        assert_eq!(editor.text(), "one\ntwo", "the delete must not have run");
    }

    #[test]
    fn a_clipboard_paste_waits_for_the_text_before_touching_the_document() {
        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        let request = editor
            .clipboard_request()
            .expect("the editor asked for a read");
        assert!(!request.over_selection);

        // Nothing has been inserted yet: the core cannot read the clipboard.
        assert_eq!(editor.text(), "one two");

        editor.supply_clipboard("X".to_string());
        assert_eq!(editor.clipboard_request(), None);
        assert_eq!(editor.text(), "one X two");
    }

    #[test]
    fn a_failed_clipboard_read_leaves_the_document_alone() {
        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.report_clipboard_failure("pbpaste is missing");
        assert_eq!(editor.clipboard_request(), None);
        assert_eq!(editor.text(), "one two", "nothing may be inserted");
        assert!(editor.message.contains("Clipboard read failed"));
    }

    #[test]
    fn an_empty_clipboard_is_not_an_error() {
        let mut editor = Editor::from_text("one two");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.supply_clipboard(String::new());
        assert_eq!(editor.text(), "one two");
        assert_eq!(editor.message, "Clipboard is empty");
    }

    #[test]
    fn a_clipboard_paste_over_a_selection_keeps_the_selection_until_the_read_succeeds() {
        let mut editor = Editor::from_text("hello world");
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('l'), Key::Char('l')],
        );
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        let request = editor
            .clipboard_request()
            .expect("the editor asked for a read");
        assert!(request.over_selection, "the selection is the target");
        assert_eq!(
            editor.text(),
            "hello world",
            "a failed read must not delete the selection"
        );

        // The text starts exactly where the selection did.
        editor.supply_clipboard("XYZ".to_string());
        assert_eq!(editor.text(), "XYZlo world");
    }

    #[test]
    fn pasting_over_a_selection_is_one_undo_step() {
        let mut editor = Editor::from_text("aaa\nbbb");
        keys(&mut editor, &[Key::Char('y'), Key::Char('y')]);
        keys(
            &mut editor,
            &[Key::Char('v'), Key::Char('l'), Key::Char('l')],
        );
        keys(&mut editor, &[Key::Char('p')]);
        // The selected row is replaced outright: the linewise paste takes the
        // row's place rather than being pushed past it.
        assert_eq!(editor.text(), "aaa\nbbb");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "aaa\nbbb", "one undo restores the selection");
    }

    #[test]
    fn multi_line_clipboard_text_pastes_linewise() {
        let mut editor = Editor::from_text("tail\n");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.supply_clipboard("head\n".to_string());
        assert_eq!(editor.text(), "tail\nhead\n");
    }

    #[test]
    fn a_linewise_clipboard_paste_without_a_trailing_break_still_gets_one() {
        let mut editor = Editor::from_text("tail\n");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.supply_clipboard("head\nmiddle".to_string());
        assert_eq!(
            editor.text(),
            "tail\nhead\nmiddle\n",
            "a linewise paste must not run into the next line"
        );
    }

    #[test]
    fn escape_clears_a_pending_register_prefix() {
        let mut editor = Editor::from_text("one\ntwo");
        keys(&mut editor, &[Key::Char('"')]);
        assert!(editor.is_waiting_for_character());
        keys(&mut editor, &[Key::Escape]);
        assert!(!editor.is_waiting_for_character());
        // `d` after Escape is an operator again, not a register name to swallow.
        keys(&mut editor, &[Key::Char('d')]);
        assert_eq!(editor.mode, Mode::OperatorPending(Operator::Delete));
        assert_eq!(editor.text(), "one\ntwo");
    }

    #[test]
    fn regex_substitution_replaces_every_match_and_counts_them() {
        let mut editor = Editor::from_text("one two one one");
        let substitution = Substitution::parse("s/one/1/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 3);
        assert_eq!(editor.text(), "1 two 1 1");
    }

    #[test]
    fn regex_substitution_without_g_replaces_only_the_first() {
        let mut editor = Editor::from_text("one two one");
        let substitution = Substitution::parse("s/one/1/").unwrap();
        assert_eq!(editor.substitute(&substitution), 1);
        assert_eq!(editor.text(), "1 two one");
    }

    #[test]
    fn regex_substitution_is_one_undo_step() {
        let mut editor = Editor::from_text("a1b1c");
        let substitution = Substitution::parse(r"s/[0-9]/X/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 2);
        assert_eq!(editor.text(), "aXbXc");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "a1b1c", "one undo rewinds every match");
    }

    #[test]
    fn regex_substitution_handles_multibyte_matches() {
        let mut editor = Editor::from_text("世界 世界");
        let substitution = Substitution::parse("s/世界/hi/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 2);
        assert_eq!(editor.text(), "hi hi");
        keys(&mut editor, &[Key::Char('u')]);
        assert_eq!(editor.text(), "世界 世界");
    }

    #[test]
    fn regex_substitution_handles_matches_of_differing_length() {
        let mut editor = Editor::from_text("aa aaa");
        let substitution = Substitution::parse("s/a+/X/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 2);
        assert_eq!(editor.text(), "X X");
    }

    #[test]
    fn regex_line_anchors_reach_every_line() {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        let substitution = Substitution::parse("s/^/> /g").unwrap();
        assert_eq!(editor.substitute(&substitution), 3);
        assert_eq!(editor.text(), "> one\n> two\n> three");
    }

    #[test]
    fn regex_dot_does_not_cross_a_line() {
        let mut editor = Editor::from_text("abc a\nc");
        let substitution = Substitution::parse("s/a.c/X/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 1);
        assert_eq!(editor.text(), "X a\nc");
    }

    #[test]
    fn regex_alternation_works() {
        let mut editor = Editor::from_text("cat dog cat");
        let substitution = Substitution::parse("s/cat|dog/pet/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 3);
        assert_eq!(editor.text(), "pet pet pet");
    }

    #[test]
    fn regex_character_classes_work() {
        let mut editor = Editor::from_text("a1b2c3");
        let substitution = Substitution::parse("s/[0-9]/_/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 3);
        assert_eq!(editor.text(), "a_b_c_", "all three digits are replaced");
    }

    #[test]
    fn regex_substitution_that_changes_nothing_leaves_the_buffer_clean() {
        let mut editor = Editor::from_text("one two one");
        editor.mark_saved();
        let substitution = Substitution::parse("s/one/one/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 2, "the text holds two");
        assert_eq!(editor.text(), "one two one");
        assert!(
            !editor.is_dirty(),
            "swapping text for itself must not rewrite the rope"
        );
    }

    #[test]
    fn an_absent_regex_pattern_is_reported() {
        let mut editor = Editor::from_text("one two");
        editor.mark_saved();
        let substitution = Substitution::parse("s/zzz/x/g").unwrap();
        assert_eq!(editor.substitute(&substitution), 0);
        assert_eq!(editor.text(), "one two");
        assert!(!editor.is_dirty());
        assert_eq!(editor.message, "Pattern not found: zzz");
    }

    #[test]
    fn a_zero_width_regex_match_inserts_at_that_point() {
        // `s/$/!/g` appends a character to every line, which is a zero-width match
        // and must not be refused the way a naive implementation would.
        let mut editor = Editor::from_text("one\ntwo");
        let substitution = Substitution::parse("s/$/!/g").unwrap();
        // Two matches: before the line break and at the end of the text. A file
        // that ended in a newline would have a third, for the empty last line.
        assert_eq!(editor.substitute(&substitution), 2);
        assert_eq!(editor.text(), "one!\ntwo!");
    }

    /// The matcher reports byte offsets and the rope is indexed in characters.
    ///
    /// Compare the conversion against counting characters the obvious way, on
    /// documents where multi-byte characters sit both inside and between matches,
    /// which is where an off-by-one would hide.
    #[test]
    fn byte_ranges_become_the_right_character_ranges() {
        for text in [
            "one two one",
            "世界 世界 世界",
            "aé漢b c",
            "🌍🌍ab",
            "mixed 世界 and é and plain",
            "no matches here",
        ] {
            for pattern in ["one", "世", "a", "[aeé]", "..", "\\w+"] {
                let Ok(substitution) = Substitution::parse(&format!("s/{pattern}/X/g")) else {
                    continue;
                };
                let byte_ranges = substitution.match_ranges(text);
                let ranges = to_char_ranges(text, &byte_ranges);

                let expected: Vec<(usize, usize)> = byte_ranges
                    .iter()
                    .map(|range| {
                        let start = text[..range.start].chars().count();
                        let end = start + text[range.start..range.end].chars().count();
                        (start, end)
                    })
                    .collect();
                let actual: Vec<(usize, usize)> = ranges
                    .iter()
                    .map(|range| (range.start.0, range.end.0))
                    .collect();
                assert_eq!(
                    actual, expected,
                    "range conversion disagreed for /{pattern}/ in {text:?}"
                );
                // And the slices themselves must line up.
                for (range, (start, end)) in ranges.iter().zip(&expected) {
                    assert_eq!(range.start.0, *start);
                    assert_eq!(range.end.0, *end);
                }
            }
        }
    }

    #[test]
    fn a_linewise_paste_onto_the_last_line_starts_a_new_line() {
        // The file has no trailing break, so there is no line after the last one
        // for `p` to land on. Without care the pasted text runs into it.
        let mut editor = Editor::from_text("tail");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.supply_clipboard("head\n".to_string());
        assert_eq!(editor.text(), "tail\nhead\n");

        // And the same for a register that already holds the text.
        let mut editor = Editor::from_text("first");
        keys(&mut editor, &[Key::Char('y'), Key::Char('y')]);
        editor.cursor = CharPos(0);
        keys(&mut editor, &[Key::Char('p')]);
        assert_eq!(editor.text(), "first\nfirst\n");
    }

    #[test]
    fn a_linewise_paste_before_the_last_line_is_unchanged() {
        // A line in the middle already has a break after it to land on.
        let mut editor = Editor::from_text("a\nb");
        editor.cursor = CharPos(0);
        keys(&mut editor, &[Key::Char('y'), Key::Char('y')]);
        editor.cursor = CharPos(0);
        keys(&mut editor, &[Key::Char('p')]);
        assert_eq!(editor.text(), "a\na\nb");
    }

    #[test]
    fn a_linewise_paste_onto_an_empty_buffer_adds_no_leading_blank() {
        // Nothing precedes the paste, so there is nothing to separate it from.
        let mut editor = Editor::from_text("");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.supply_clipboard("head\n".to_string());
        assert_eq!(editor.text(), "head\n");
    }

    #[test]
    fn a_linewise_paste_onto_a_file_that_already_ends_in_a_break_is_unchanged() {
        let mut editor = Editor::from_text("tail\n");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.supply_clipboard("head\n".to_string());
        assert_eq!(editor.text(), "tail\nhead\n");
    }

    #[test]
    fn a_linewise_paste_before_the_last_line_needs_no_new_line() {
        // `P` puts the text above, which always has a line to start on.
        let mut editor = Editor::from_text("tail");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('P')],
        );
        editor.supply_clipboard("head\n".to_string());
        assert_eq!(editor.text(), "head\ntail");
    }

    #[test]
    fn a_characterwise_paste_onto_the_last_line_still_abuts_it() {
        // Characterwise text is meant to sit inline, so nothing changes here.
        let mut editor = Editor::from_text("tail");
        keys(
            &mut editor,
            &[Key::Char('"'), Key::Char('+'), Key::Char('p')],
        );
        editor.supply_clipboard("more".to_string());
        assert_eq!(editor.text(), "tailmore");
    }

    #[test]
    fn replacing_the_whole_document_advances_the_revision_past_every_earlier_value() {
        let mut editor = Editor::from_text("abc");
        // The renderer keys its caches on the revision, so the value has to be
        // strictly increasing for the lifetime of the process. Resetting it to
        // zero let a second `:e` reuse entries recorded for the first file,
        // because an unmodified buffer sits at revision zero on both occasions.
        editor.replace_text("(abc)");
        let after_first = editor.revision();
        editor.replace_text("(def)");
        assert_ne!(
            editor.revision(),
            after_first,
            "replace_text must advance the revision, not restart it"
        );
        editor.replace_text("(ghi)");
        assert!(
            editor.revision() > after_first,
            "the revision must never return to a value the caches have already seen"
        );
        assert!(!editor.is_dirty(), "a freshly opened file is not dirty");
    }
}
