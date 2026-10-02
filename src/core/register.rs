//! Registers.
//!
//! The development plan scopes v0.1 to three registers: the unnamed register that
//! every yank and delete writes to, the black hole that accepts a delete and
//! discards it, and the system clipboard. Named registers and the yank register
//! are explicitly later work.
//!
//! A register records the *shape* of its text alongside the text, because that
//! decides how a paste behaves: linewise text goes on its own line, characterwise
//! text lands inline, and getting that wrong is visible immediately.

/// Whether the text in a register is character- or line-shaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterKind {
    Characterwise,
    Linewise,
    Blockwise,
}

/// Text held in a register, and how it should be put back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterValue {
    pub text: String,
    pub kind: RegisterKind,
}

impl RegisterValue {
    pub fn new(text: String, kind: RegisterKind) -> Self {
        Self { text, kind }
    }

    pub fn characterwise(text: String) -> Self {
        Self::new(text, RegisterKind::Characterwise)
    }

    /// True when the text should be placed on lines of its own.
    pub fn is_linewise(&self) -> bool {
        !matches!(self.kind, RegisterKind::Characterwise)
    }
}

/// Which register a command reads from or writes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Register {
    /// The default. Also spelled `"-`.
    Unnamed,
    /// Accepts a delete and throws it away, so the unnamed register is left
    /// alone. Spelled `"_`.
    BlackHole,
    /// The system clipboard. Spelled `"+`, and `"*` as an alias.
    Clipboard,
}

impl Register {
    /// Resolves the key after a `"`.
    ///
    /// Returns `None` for a register this version does not have, which aborts
    /// the command rather than falling back to the unnamed register. Running
    /// `"dd` because `d` was mistaken for a register name would delete a line
    /// into the wrong place, so failing is the safe answer.
    ///
    /// `*` aliases `+` rather than naming the X11 primary selection: the plan
    /// scopes both to the system clipboard, and macOS has no primary selection.
    pub fn from_key(key: char) -> Option<Self> {
        match key {
            '-' => Some(Register::Unnamed),
            '_' => Some(Register::BlackHole),
            '+' | '*' => Some(Register::Clipboard),
            _ => None,
        }
    }

    /// Whether a write to this register has to reach outside the editor.
    pub fn is_external(self) -> bool {
        matches!(self, Register::Clipboard)
    }

    /// Whether a read from this register has to reach outside the editor.
    ///
    /// The black hole always reads as empty, so it never needs to.
    pub fn needs_external_read(self) -> bool {
        matches!(self, Register::Clipboard)
    }
}

/// The registers the editor holds in memory.
///
/// The system clipboard is not here: it belongs to the operating system, and
/// reading or writing it is the application layer's job. See
/// [`Register::is_external`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    unnamed: Option<RegisterValue>,
}

impl Registers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unnamed(&self) -> Option<&RegisterValue> {
        self.unnamed.as_ref()
    }

    /// Stores `value` unless the target is the black hole.
    pub fn write(&mut self, target: Register, value: RegisterValue) {
        match target {
            Register::Unnamed => self.unnamed = Some(value),
            // The black hole exists precisely to not keep anything.
            Register::BlackHole => {}
            // Handled by the caller, which owns the clipboard connection.
            Register::Clipboard => {}
        }
    }

    pub fn clear(&mut self) {
        self.unnamed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{Register, RegisterKind, RegisterValue, Registers};

    fn value(text: &str) -> RegisterValue {
        RegisterValue::characterwise(text.to_string())
    }

    #[test]
    fn resolves_the_documented_register_keys() {
        assert_eq!(Register::from_key('-'), Some(Register::Unnamed));
        assert_eq!(Register::from_key('_'), Some(Register::BlackHole));
        assert_eq!(Register::from_key('+'), Some(Register::Clipboard));
        assert_eq!(Register::from_key('*'), Some(Register::Clipboard));
    }

    #[test]
    fn rejects_register_keys_it_does_not_have() {
        // A named register is later work; falling back to the unnamed register
        // would run the command against the wrong target.
        for key in ['a', '0', '1', '"', '/', ':'] {
            assert_eq!(Register::from_key(key), None, "{key:?} should be rejected");
        }
    }

    #[test]
    fn the_black_hole_discards_what_is_written_to_it() {
        let mut registers = Registers::new();
        registers.write(Register::Unnamed, value("kept"));
        assert_eq!(registers.unnamed().map(|v| v.text.as_str()), Some("kept"));

        registers.write(Register::BlackHole, value("discarded"));
        assert_eq!(
            registers.unnamed().map(|v| v.text.as_str()),
            Some("kept"),
            "the black hole must not overwrite the unnamed register"
        );
    }

    #[test]
    fn linewise_values_are_recognised() {
        assert!(RegisterValue::new(String::new(), RegisterKind::Linewise).is_linewise());
        assert!(RegisterValue::new(String::new(), RegisterKind::Blockwise).is_linewise());
        assert!(!RegisterValue::characterwise(String::new()).is_linewise());
    }

    #[test]
    fn only_the_clipboard_reaches_outside_the_editor() {
        assert!(Register::Clipboard.is_external());
        assert!(Register::Clipboard.needs_external_read());
        assert!(!Register::BlackHole.needs_external_read());
        assert!(!Register::Unnamed.is_external());
    }
}
