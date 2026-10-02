pub mod document;
pub mod editor;
pub mod history;
pub mod motion;
pub mod pairing;
pub mod position;
pub mod register;
pub mod substitute;
pub mod textobject;

pub use document::Document;
pub use editor::{
    Editor, EditorOutcome, Key, Mode, Operator, PasteRequest, Selection, SelectionKind,
};
pub use history::{Edit, History, DEFAULT_UNDO_BUDGET};
pub use motion::{resolve as resolve_motion, Motion, MotionResult};
pub use pairing::PairIndex;
pub use position::{CharPos, CharRange, LineColumn, RangeKind};
pub use register::{Register, RegisterKind, RegisterValue, Registers};
pub use substitute::Substitution;
pub use textobject::{
    matching_partner, matching_partner_within, resolve as resolve_text_object, PairKind,
    TextObject, PARTNER_SCAN_LIMIT,
};
