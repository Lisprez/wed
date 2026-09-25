use super::document::Document;
use super::history::{Edit, History};
use super::motion::{self, Motion, MotionResult};
use super::position::{CharPos, CharRange, RangeKind};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegisterKind {
    Characterwise,
    Linewise,
    Blockwise,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegisterValue {
    text: String,
    kind: RegisterKind,
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

pub struct Editor {
    pub document: Document,
    pub cursor: CharPos,
    pub mode: Mode,
    pub selection: Option<Selection>,
    pub message: String,
    search_input: String,
    search_backwards: bool,
    last_search: Option<String>,
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
    unnamed_register: Option<RegisterValue>,
    pending_clipboard_text: Option<String>,
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
            unnamed_register: None,
            pending_clipboard_text: None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    pub fn commit_pending_edit(&mut self) {
        self.history.commit();
    }

    pub fn handle_key(&mut self, key: Key) -> EditorOutcome {
        self.pending_clipboard_text = None;
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
        self.document = Document::from_text(text);
        self.cursor = CharPos::ZERO;
        self.mode = Mode::Normal;
        self.selection = None;
        self.history = History::new();
        self.revision = 0;
        self.saved_revision = 0;
        self.normal_count = None;
        self.pending = None;
        self.visual_prefix = None;
        self.find_pending = None;
        self.g_pending = false;
        self.replace_pending = false;
        self.search_input.clear();
        self.last_search = None;
        self.message.clear();
        self.pending_clipboard_text = None;
    }

    pub fn replace_literal(&mut self, needle: &str, replacement: &str, global: bool) -> usize {
        if needle.is_empty() {
            return 0;
        }
        let original = self.text();
        let count = if global {
            original.matches(needle).count()
        } else if original.contains(needle) {
            1
        } else {
            0
        };
        if count == 0 {
            self.message = format!("Pattern not found: {}", needle);
            return 0;
        }
        let replaced = if global {
            original.replace(needle, replacement)
        } else {
            original.replacen(needle, replacement, 1)
        };
        let range = CharRange::new(CharPos::ZERO, CharPos(self.document.len_chars()));
        self.history.begin();
        self.apply_edit(range, &replaced);
        self.history.commit();
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

    fn handle_normal_key(&mut self, key: Key) -> EditorOutcome {
        if self.replace_pending {
            if let Key::Char(character) = key {
                self.replace_pending = false;
                self.replace_current(character);
            } else if key == Key::Escape {
                self.replace_pending = false;
            }
            return EditorOutcome::Continue;
        }
        if self.g_pending {
            self.g_pending = false;
            if key == Key::Char('g') {
                let count = self.take_count();
                self.move_with(Motion::GotoFirst, count);
            }
            return EditorOutcome::Continue;
        }
        if let Key::Char(character) = key {
            if self.accumulate_normal_digit(character) {
                return EditorOutcome::Continue;
            }
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
            Key::Char('G') => {
                let count = self.take_count();
                if count == 1 {
                    self.move_with(Motion::GotoLast, 1);
                } else {
                    let line = count.saturating_sub(1);
                    self.cursor = self.document.line_start(line);
                    self.selection = None;
                }
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
            Key::Char('f') => {
                self.find_pending = Some(FindState {
                    kind: FindKind::Forward,
                    count: self.take_count(),
                })
            }
            Key::Char('F') => {
                self.find_pending = Some(FindState {
                    kind: FindKind::Backward,
                    count: self.take_count(),
                })
            }
            Key::Char('t') => {
                self.find_pending = Some(FindState {
                    kind: FindKind::TillForward,
                    count: self.take_count(),
                })
            }
            Key::Char('T') => {
                self.find_pending = Some(FindState {
                    kind: FindKind::TillBackward,
                    count: self.take_count(),
                })
            }
            Key::Char(';') => self.repeat_find(false),
            Key::Char(',') => self.repeat_find(true),
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
                    if let Some(target) =
                        find_search(&self.document, self.cursor, &query, self.search_backwards)
                    {
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
        if let Some(target) = find_search(&self.document, self.cursor, &query, backwards) {
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
            Key::Escape => {
                self.mode = Mode::Normal;
                self.selection = None;
            }
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
                }
            }
            Key::Char('d') => self.apply_visual_operator(Operator::Delete),
            Key::Char('c') => self.apply_visual_operator(Operator::Change),
            Key::Char('y') => self.apply_visual_operator(Operator::Yank),
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
        let Some(mut pending) = self.pending else {
            self.mode = Mode::Normal;
            return EditorOutcome::Continue;
        };
        if let Some(prefix) = pending.prefix {
            if let Key::Char(character) = key {
                if let Some(object) =
                    textobject::parse(if prefix.around { 'a' } else { 'i' }, character)
                {
                    let count = pending
                        .count
                        .saturating_mul(pending.motion_count.unwrap_or(1));
                    self.pending = None;
                    self.apply_text_object(pending.operator, object, count);
                } else {
                    self.pending = None;
                    self.mode = Mode::Normal;
                    self.message = "Unknown text object".to_string();
                }
            }
            return EditorOutcome::Continue;
        }
        if let Key::Char(character) = key {
            if character.is_ascii_digit() && (character != '0' || pending.motion_count.is_some()) {
                pending.motion_count = Some(
                    pending
                        .motion_count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(character.to_digit(10).unwrap_or(0) as usize),
                );
                self.pending = Some(pending);
                return EditorOutcome::Continue;
            }
            if pending.g_pending {
                pending.g_pending = false;
                if character == 'g' {
                    self.pending = None;
                    self.apply_line_motion(pending.operator, pending.count, Motion::GotoFirst);
                    return EditorOutcome::Continue;
                }
                self.pending = None;
                self.mode = Mode::Normal;
                return EditorOutcome::Continue;
            }
            if operator_for_key(character) == Some(pending.operator) {
                self.pending = None;
                self.apply_line_operator(pending.operator, pending.count);
                return EditorOutcome::Continue;
            }
            if character == 'i' || character == 'a' {
                pending.prefix = Some(ObjectPrefix {
                    around: character == 'a',
                });
                self.pending = Some(pending);
                return EditorOutcome::Continue;
            }
            if character == 'g' {
                pending.g_pending = true;
                self.pending = Some(pending);
                return EditorOutcome::Continue;
            }
            if character == 'G' {
                self.pending = None;
                let count = pending
                    .count
                    .saturating_mul(pending.motion_count.unwrap_or(1));
                self.apply_line_motion(pending.operator, count, Motion::GotoLast);
                return EditorOutcome::Continue;
            }
            if let Some(motion) = motion_for_key(character) {
                self.pending = None;
                let count = pending
                    .count
                    .saturating_mul(pending.motion_count.unwrap_or(1));
                let result = motion::resolve(&self.document, self.cursor, motion, count);
                self.apply_motion_result(pending.operator, result, count);
                return EditorOutcome::Continue;
            }
        }
        self.pending = None;
        self.mode = Mode::Normal;
        EditorOutcome::Continue
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
        } else if let Key::Char(character) = key {
            if let Some(state) = self.find_pending.take() {
                self.resolve_find(state, character);
            }
        }
    }

    fn move_with(&mut self, motion: Motion, count: usize) {
        let result = motion::resolve(&self.document, self.cursor, motion, count);
        self.cursor = result.target;
        self.selection = None;
    }

    fn update_visual(&mut self, motion: Motion) {
        let count = self.take_count();
        let Some(active) = self.selection.as_ref().map(|selection| selection.active) else {
            return;
        };
        let result = motion::resolve(&self.document, active, motion, count);
        let column = self.document.pos_to_line_col(result.target).column;
        if let Some(selection) = &mut self.selection {
            selection.active = result.target;
            if let SelectionKind::Blockwise { left, right } = &mut selection.kind {
                *left = (*left).min(column);
                *right = (*right).max(column + 1);
            }
        }
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

    fn put(&mut self, before: bool) {
        let Some(register) = self.unnamed_register.clone() else {
            self.message = "Register is empty".to_string();
            return;
        };
        let mut text = register.text.clone();
        let position = match register.kind {
            RegisterKind::Characterwise => {
                let position = self.characterwise_put_position(before);
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
            }
            RegisterKind::Linewise | RegisterKind::Blockwise => {
                let line = self.document.pos_to_line_col(self.cursor).line;
                if before {
                    self.document.line_start(line)
                } else {
                    self.document.line_end_with_newline(line)
                }
            }
        };
        self.apply_edit(CharRange::empty(position), &text);
        self.cursor = position.advance(text.chars().count());
    }

    fn characterwise_put_position(&self, before: bool) -> CharPos {
        if before || self.cursor.0 >= self.document.len_chars() {
            return self.cursor;
        }
        let character = self.document.char_at(self.cursor);
        if character.is_some_and(char::is_whitespace) {
            return self.cursor.advance(1).clamp(self.document.len_chars());
        }
        let mut end = self.cursor.advance(1);
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
        let last_line = (line + count.saturating_sub(1))
            .min(document_line_count(&self.document).saturating_sub(1));
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
        match operator {
            Operator::Yank => {
                self.unnamed_register = Some(RegisterValue {
                    text,
                    kind: register_kind(kind),
                });
                self.message = format!("Yanked {} characters", text_len);
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Operator::Delete => {
                self.apply_edit(range, "");
                self.cursor = range.start;
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Operator::Change => {
                self.history.begin();
                self.apply_edit(range, "");
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

    fn apply_visual_operator(&mut self, operator: Operator) {
        let Some(selection) = self.selection.clone() else {
            return;
        };
        let ranges = selection.ranges(&self.document);
        if ranges.is_empty() {
            self.mode = Mode::Normal;
            self.selection = None;
            return;
        }
        match operator {
            Operator::Yank => {
                let text = ranges
                    .iter()
                    .map(|range| self.document.slice(*range))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.unnamed_register = Some(RegisterValue {
                    text: text.clone(),
                    kind: register_kind(match selection.kind {
                        SelectionKind::Characterwise => RangeKind::Characterwise,
                        SelectionKind::Linewise => RangeKind::Linewise,
                        SelectionKind::Blockwise { .. } => RangeKind::Blockwise,
                    }),
                });
                self.pending_clipboard_text = Some(text);
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Operator::Delete => {
                for range in ranges.iter().rev() {
                    self.apply_edit(*range, "");
                }
                self.cursor = ranges
                    .first()
                    .map(|range| range.start)
                    .unwrap_or(self.cursor);
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Operator::Change => {
                if ranges.len() == 1 {
                    self.history.begin();
                }
                for range in ranges.iter().rev() {
                    self.apply_edit(*range, "");
                }
                self.cursor = ranges
                    .first()
                    .map(|range| range.start)
                    .unwrap_or(self.cursor);
                self.mode = Mode::Insert;
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
        let original = self.document.slice(line_range);
        let mut updated = String::new();
        for line in original.split_inclusive('\n') {
            if increase {
                updated.push_str("    ");
                updated.push_str(line);
            } else {
                let mut removed = 0;
                let mut chars = line.chars();
                while removed < 4 {
                    match chars.next() {
                        Some(' ') | Some('\t') => removed += 1,
                        _ => break,
                    }
                }
                updated.push_str(&line.chars().skip(removed).collect::<String>());
            }
        }
        self.apply_edit(line_range, &updated);
    }

    fn apply_edit(&mut self, range: CharRange, inserted: &str) {
        let start = range.start;
        let actual_removed = self.document.replace_range(range, inserted);
        if actual_removed.is_empty() && inserted.is_empty() {
            return;
        }
        if actual_removed == inserted {
            return;
        }
        self.history.record(Edit {
            start,
            removed: actual_removed,
            inserted: inserted.to_string(),
        });
        self.revision = self.revision.saturating_add(1);
    }

    fn undo(&mut self) {
        if let Some(transaction) = self.history.undo(&mut self.document) {
            if let Some(edit) = transaction.edits.first() {
                self.cursor = edit.start;
            }
            self.revision = self.revision.saturating_add(1);
            self.mode = Mode::Normal;
            self.selection = None;
        }
    }

    fn redo(&mut self) {
        if let Some(transaction) = self.history.redo(&mut self.document) {
            if let Some(edit) = transaction.edits.last() {
                self.cursor = edit.start.advance(edit.inserted.chars().count());
            }
            self.revision = self.revision.saturating_add(1);
            self.mode = Mode::Normal;
            self.selection = None;
        }
    }

    fn resolve_find(&mut self, state: FindState, character: char) {
        self.last_find = Some((state.kind, character));
        let target = find_target(
            &self.document,
            self.cursor,
            state.kind,
            character,
            state.count,
        );
        if let Some(target) = target {
            self.cursor = target;
        }
    }

    fn repeat_find(&mut self, reverse: bool) {
        let Some((kind, character)) = self.last_find else {
            return;
        };
        let kind = if reverse {
            match kind {
                FindKind::Forward => FindKind::Backward,
                FindKind::Backward => FindKind::Forward,
                FindKind::TillForward => FindKind::TillBackward,
                FindKind::TillBackward => FindKind::TillForward,
            }
        } else {
            kind
        };
        self.resolve_find(FindState { kind, count: 1 }, character);
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

    fn reset_pending(&mut self) {
        self.normal_count = None;
        self.pending = None;
        self.find_pending = None;
        self.g_pending = false;
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
        10
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

fn register_kind(kind: RangeKind) -> RegisterKind {
    match kind {
        RangeKind::Characterwise => RegisterKind::Characterwise,
        RangeKind::Linewise => RegisterKind::Linewise,
        RangeKind::Blockwise => RegisterKind::Blockwise,
    }
}

fn find_search(
    document: &Document,
    cursor: CharPos,
    query: &str,
    backwards: bool,
) -> Option<CharPos> {
    let needle: Vec<char> = query.chars().collect();
    if needle.is_empty() || needle.len() > document.len_chars() {
        return None;
    }
    let len = document.len_chars();
    if backwards {
        if cursor.0 == 0 {
            return None;
        }
        let mut start = cursor.0.min(len);
        while start > 0 {
            start -= 1;
            if start + needle.len() <= len
                && (0..needle.len())
                    .all(|offset| document.char_at(CharPos(start + offset)) == Some(needle[offset]))
            {
                return Some(CharPos(start));
            }
        }
    } else {
        let mut start = cursor.0.saturating_add(1);
        while start + needle.len() <= len {
            if (0..needle.len())
                .all(|offset| document.char_at(CharPos(start + offset)) == Some(needle[offset]))
            {
                return Some(CharPos(start));
            }
            start += 1;
        }
    }
    None
}

fn find_target(
    document: &Document,
    cursor: CharPos,
    kind: FindKind,
    character: char,
    count: usize,
) -> Option<CharPos> {
    let len = document.len_chars();
    let count = count.max(1);
    match kind {
        FindKind::Forward | FindKind::TillForward => {
            let mut index = cursor.0.saturating_add(1);
            let mut found = 0;
            while index < len {
                if document.char_at(CharPos(index)) == Some(character) {
                    found += 1;
                    if found == count {
                        return Some(
                            if matches!(kind, FindKind::TillForward) && index > cursor.0 {
                                CharPos(index - 1)
                            } else {
                                CharPos(index)
                            },
                        );
                    }
                }
                index += 1;
            }
            None
        }
        FindKind::Backward | FindKind::TillBackward => {
            if cursor.0 == 0 {
                return None;
            }
            let mut index = cursor.0 - 1;
            let mut found = 0;
            loop {
                if document.char_at(CharPos(index)) == Some(character) {
                    found += 1;
                    if found == count {
                        return Some(
                            if matches!(kind, FindKind::TillBackward) && index + 1 < len {
                                CharPos(index + 1)
                            } else {
                                CharPos(index)
                            },
                        );
                    }
                }
                if index == 0 {
                    break;
                }
                index -= 1;
            }
            None
        }
    }
}

fn document_line_count(document: &Document) -> usize {
    document.line_count()
}

#[cfg(test)]
mod tests {
    use super::{Editor, Key, Mode};
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
}
