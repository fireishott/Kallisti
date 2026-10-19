//! Prompt compilation. Skeleton only in Wave 0.
//!
//! The block order below is the default from locked amendment 12, pending its
//! own ADR. Two properties matter and are encoded here rather than left to a
//! comment in the compiler:
//!
//! - lessons never sit in front of tools, and
//! - the tool block order is frozen.

/// One block of the compiled prompt, in the order it is assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BlockKind {
    /// Who the assistant is for this customer.
    Persona,
    /// Product rules the customer cannot override.
    ProductPolicy,
    /// Tool schemas. Order inside this block is frozen.
    ToolSchemas,
    /// Which skills exist.
    SkillCatalog,
    /// Workspace facts and files.
    Workspace,
    /// Lessons pinned to the current session.
    PinnedLessons,
    /// Anything volatile: time, cursor, recent turns.
    VolatileTail,
}

/// The default block order (locked amendment 12).
pub const DEFAULT_BLOCK_ORDER: [BlockKind; 7] = [
    BlockKind::Persona,
    BlockKind::ProductPolicy,
    BlockKind::ToolSchemas,
    BlockKind::SkillCatalog,
    BlockKind::Workspace,
    BlockKind::PinnedLessons,
    BlockKind::VolatileTail,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lessons_never_precede_tools() {
        let tools = DEFAULT_BLOCK_ORDER
            .iter()
            .position(|kind| *kind == BlockKind::ToolSchemas)
            .expect("tool block is always present");
        let lessons = DEFAULT_BLOCK_ORDER
            .iter()
            .position(|kind| *kind == BlockKind::PinnedLessons)
            .expect("lesson block is always present");
        assert!(tools < lessons);
    }

    #[test]
    fn volatile_tail_is_last() {
        assert_eq!(DEFAULT_BLOCK_ORDER.last(), Some(&BlockKind::VolatileTail));
    }
}
