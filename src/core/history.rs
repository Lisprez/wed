use super::document::Document;
use super::position::{CharPos, CharRange};
use std::collections::VecDeque;

/// Upper bound on the text the undo stack may hold.
///
/// The development plan targets a resident size close to one or two times the
/// file. Undo text is pure overhead on top of that, and it grows without limit:
/// a session of ordinary edits accumulates slowly, and a single whole-document
/// rewrite such as `:s///g` drops two more copies of the document into one
/// transaction at once.
///
/// Dropping the oldest transactions once the budget is exceeded bounds it. That
/// is the same trade every editor makes, and it is bounded loss: undo reaches
/// back as far as the budget allows rather than as far as the session did.
///
/// The most recent transaction is always kept, whatever its size. Without that,
/// an edit larger than the whole budget would silently become non-undoable.
pub const DEFAULT_UNDO_BUDGET: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: CharPos,
    pub removed: String,
    pub inserted: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub edits: Vec<Edit>,
}

impl Transaction {
    fn bytes(&self) -> usize {
        self.edits
            .iter()
            .map(|edit| edit.removed.len() + edit.inserted.len())
            .sum()
    }
}

pub struct History {
    /// Oldest first, so the transaction furthest in the past is the one dropped
    /// when the budget is exceeded.
    undo_stack: VecDeque<Transaction>,
    redo_stack: Vec<Transaction>,
    active: Option<Transaction>,
    budget: usize,
    /// Text currently held by both stacks. Moving a transaction between them does
    /// not change this.
    held: usize,
}

impl History {
    pub fn new() -> Self {
        Self::with_budget(DEFAULT_UNDO_BUDGET)
    }

    pub fn with_budget(budget: usize) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            active: None,
            budget,
            held: 0,
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Changes the budget and discards whatever now exceeds it.
    ///
    /// The existing undo history is kept as far as the new budget allows, so
    /// raising it never loses anything and lowering it loses only the oldest
    /// transactions.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.enforce_budget();
    }

    /// Bytes of undoable text currently retained.
    pub fn held(&self) -> usize {
        self.held
    }

    pub fn begin(&mut self) {
        if self.active.is_none() {
            self.active = Some(Transaction { edits: Vec::new() });
        }
    }

    pub fn record(&mut self, edit: Edit) {
        if let Some(transaction) = &mut self.active {
            transaction.edits.push(edit);
        } else {
            let transaction = Transaction { edits: vec![edit] };
            self.push_undo(transaction);
        }
    }

    pub fn commit(&mut self) {
        let Some(transaction) = self.active.take() else {
            return;
        };
        if transaction.edits.is_empty() {
            return;
        }
        self.push_undo(transaction);
    }

    /// Reverses the most recent transaction and returns the position the cursor
    /// should move to, if there was one.
    ///
    /// The transaction itself moves to the redo stack rather than being copied,
    /// so undoing a large edit does not duplicate its text.
    pub fn undo(&mut self, document: &mut Document) -> Option<CharPos> {
        let transaction = self.undo_stack.pop_back()?;
        for edit in transaction.edits.iter().rev() {
            let current = CharRange::new(
                edit.start,
                edit.start.advance(edit.inserted.chars().count()),
            );
            document.overwrite_range(current, &edit.removed);
        }
        let cursor = transaction.edits.first().map(|edit| edit.start);
        self.redo_stack.push(transaction);
        self.enforce_budget();
        cursor
    }

    /// Reapplies the most recently undone transaction and returns the position
    /// the cursor should move to, if there was one.
    pub fn redo(&mut self, document: &mut Document) -> Option<CharPos> {
        let transaction = self.redo_stack.pop()?;
        for edit in &transaction.edits {
            let current =
                CharRange::new(edit.start, edit.start.advance(edit.removed.chars().count()));
            document.overwrite_range(current, &edit.inserted);
        }
        let cursor = transaction
            .edits
            .last()
            .map(|edit| edit.start.advance(edit.inserted.chars().count()));
        self.undo_stack.push_back(transaction);
        self.enforce_budget();
        cursor
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.active = None;
        self.held = 0;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn len(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.undo_stack.is_empty()
    }

    /// Commits a transaction and discards the redo stack, which a fresh edit
    /// invalidates.
    fn push_undo(&mut self, transaction: Transaction) {
        self.held = self.held.saturating_add(transaction.bytes());
        self.undo_stack.push_back(transaction);
        for abandoned in self.redo_stack.drain(..) {
            self.held = self.held.saturating_sub(abandoned.bytes());
        }
        self.enforce_budget();
    }

    /// Discards the oldest transactions until the budget is met.
    ///
    /// Keeps at least one, so the most recent edit stays undoable no matter how
    /// much text it holds. Only the undo stack is trimmed: the redo stack is
    /// already capped by the number of undos the user has performed, and throwing
    /// away a redo would silently break Ctrl-R.
    fn enforce_budget(&mut self) {
        while self.held > self.budget && self.undo_stack.len() > 1 {
            let Some(oldest) = self.undo_stack.pop_front() else {
                break;
            };
            self.held = self.held.saturating_sub(oldest.bytes());
        }
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Edit, History};
    use crate::core::document::Document;
    use crate::core::position::{CharPos, CharRange};

    #[test]
    fn undoes_and_redoes_a_transaction() {
        let mut document = Document::from_text("hello world");
        let mut history = History::new();
        let removed = document.replace_range(CharRange::new(CharPos(6), CharPos(11)), "rust");
        history.record(Edit {
            start: CharPos(6),
            removed,
            inserted: "rust".to_string(),
        });
        assert_eq!(document.to_string(), "hello rust");
        history.undo(&mut document);
        assert_eq!(document.to_string(), "hello world");
        history.redo(&mut document);
        assert_eq!(document.to_string(), "hello rust");
    }

    #[test]
    fn groups_multiple_edits_until_commit() {
        let mut document = Document::from_text("a");
        let mut history = History::new();
        history.begin();
        let removed = document.replace_range(CharRange::new(CharPos(0), CharPos(0)), "b");
        history.record(Edit {
            start: CharPos(0),
            removed,
            inserted: "b".to_string(),
        });
        let removed = document.replace_range(CharRange::new(CharPos(1), CharPos(1)), "c");
        history.record(Edit {
            start: CharPos(1),
            removed,
            inserted: "c".to_string(),
        });
        history.commit();
        assert_eq!(document.to_string(), "bca");
        history.undo(&mut document);
        assert_eq!(document.to_string(), "a");
    }

    #[test]
    fn new_transaction_clears_redo_stack() {
        let mut document = Document::from_text("a");
        let mut history = History::new();
        let removed = document.replace_range(CharRange::new(CharPos(0), CharPos(1)), "b");
        history.record(Edit {
            start: CharPos(0),
            removed,
            inserted: "b".to_string(),
        });
        history.undo(&mut document);
        assert!(history.can_redo());
        let removed = document.replace_range(CharRange::new(CharPos(0), CharPos(1)), "c");
        history.record(Edit {
            start: CharPos(0),
            removed,
            inserted: "c".to_string(),
        });
        assert!(!history.can_redo());
    }

    /// Pushes one transaction of `bytes` characters onto the undo stack.
    fn push(history: &mut History, bytes: usize) {
        let text = "x".repeat(bytes);
        history.record(Edit {
            start: CharPos(0),
            removed: String::new(),
            inserted: text,
        });
    }

    #[test]
    fn drops_the_oldest_transactions_once_the_budget_is_exceeded() {
        let mut history = History::with_budget(250);
        for _ in 0..5 {
            push(&mut history, 100);
        }
        // Two hundred fits, three hundred does not, so the stack settles at two
        // transactions regardless of how many were pushed.
        assert_eq!(history.len(), 2, "oldest transactions are dropped first");
        assert!(
            history.held() <= history.budget(),
            "held {} exceeds budget {}",
            history.held(),
            history.budget()
        );
    }

    #[test]
    fn keeps_the_newest_transaction_even_when_it_exceeds_the_budget_alone() {
        let mut history = History::with_budget(10);
        push(&mut history, 1_000);
        assert_eq!(history.len(), 1, "the most recent edit stays undoable");
        assert!(history.can_undo());
    }

    #[test]
    fn the_budget_never_claims_more_than_it_holds() {
        let mut document = Document::from_text("abc");
        let mut history = History::new();
        let removed = document.replace_range(CharRange::new(CharPos(0), CharPos(1)), "z");
        history.record(Edit {
            start: CharPos(0),
            removed,
            inserted: "z".to_string(),
        });
        assert_eq!(history.held(), 2, "one character removed, one inserted");
        // Undoing moves the text to the redo stack rather than duplicating it.
        history.undo(&mut document);
        assert_eq!(history.held(), 2);
        history.redo(&mut document);
        assert_eq!(history.held(), 2);
        // A fresh edit discards the redo stack, releasing its text.
        push(&mut history, 10);
        assert_eq!(history.held(), 12);
    }

    #[test]
    fn clearing_releases_everything() {
        let mut history = History::new();
        push(&mut history, 500);
        assert!(history.held() > 0);
        history.clear();
        assert_eq!(history.held(), 0);
        assert!(!history.can_undo());
    }
}
