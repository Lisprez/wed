pub mod document;
pub mod editor;
pub mod history;
pub mod motion;
pub mod position;
pub mod textobject;

pub use document::Document;
pub use editor::{Editor, EditorOutcome, Key, Mode, Operator, Selection, SelectionKind};
pub use history::{Edit, History};
pub use motion::{resolve as resolve_motion, Motion, MotionResult};
pub use position::{CharPos, CharRange, LineColumn, RangeKind};
pub use textobject::{matching_partner, resolve as resolve_text_object, PairKind, TextObject};
