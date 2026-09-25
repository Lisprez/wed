#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CharPos(pub usize);

impl CharPos {
    pub const ZERO: Self = Self(0);

    pub fn new(index: usize) -> Self {
        Self(index)
    }

    pub fn get(self) -> usize {
        self.0
    }

    pub fn clamp(self, len: usize) -> Self {
        Self(self.0.min(len))
    }

    pub fn advance(self, amount: usize) -> Self {
        Self(self.0.saturating_add(amount))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CharRange {
    pub start: CharPos,
    pub end: CharPos,
}

impl CharRange {
    pub fn new(start: CharPos, end: CharPos) -> Self {
        if start.0 <= end.0 {
            Self { start, end }
        } else {
            Self {
                start: end,
                end: start,
            }
        }
    }

    pub fn empty(pos: CharPos) -> Self {
        Self {
            start: pos,
            end: pos,
        }
    }

    pub fn len(self) -> usize {
        self.end.0.saturating_sub(self.start.0)
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn contains(self, pos: CharPos) -> bool {
        pos >= self.start && pos < self.end
    }

    pub fn clamp(self, len: usize) -> Self {
        let end = self.end.clamp(len);
        let start = self.start.clamp(end.0);
        Self { start, end }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineColumn {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeKind {
    Characterwise,
    Linewise,
    Blockwise,
}
