use super::document::Document;
use super::position::{CharPos, CharRange};

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

pub struct History {
    undo_stack: Vec<Transaction>,
    redo_stack: Vec<Transaction>,
    active: Option<Transaction>,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            active: None,
        }
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
            self.undo_stack.push(Transaction { edits: vec![edit] });
            self.redo_stack.clear();
        }
    }

    pub fn commit(&mut self) {
        let Some(transaction) = self.active.take() else {
            return;
        };
        if !transaction.edits.is_empty() {
            self.undo_stack.push(transaction);
            self.redo_stack.clear();
        }
    }

    /// Reverses the most recent transaction and returns the position the cursor
    /// should move to, if there was one.
    ///
    /// The transaction itself moves to the redo stack rather than being copied,
    /// so undoing a large edit does not duplicate its text.
    pub fn undo(&mut self, document: &mut Document) -> Option<CharPos> {
        let transaction = self.undo_stack.pop()?;
        for edit in transaction.edits.iter().rev() {
            let current = CharRange::new(
                edit.start,
                edit.start.advance(edit.inserted.chars().count()),
            );
            document.overwrite_range(current, &edit.removed);
        }
        let cursor = transaction.edits.first().map(|edit| edit.start);
        self.redo_stack.push(transaction);
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
        self.undo_stack.push(transaction);
        cursor
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.active = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
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
}
