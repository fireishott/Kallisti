//! Prompt compilation.
//!
//! The block order below is the default from locked amendment 12, pending its
//! own ADR. Two properties matter and are encoded here rather than left to a
//! comment in the compiler:
//!
//! - lessons never sit in front of tools, and
//! - the tool block order is frozen.
//!
//! [`compile`] is the one function that turns a [`PromptInputs`] into the system
//! text a turn sends. Four properties are deliberate and tested:
//!
//! - blocks are emitted in [`DEFAULT_BLOCK_ORDER`], so the persona is always
//!   first among the blocks that exist;
//! - an empty block is omitted entirely. A heading with nothing under it is a
//!   instruction to the model to invent something, and it is never emitted;
//! - the output is a pure function of its input. No clock, no counter, no map
//!   iteration order: the same inputs are the same bytes;
//! - the text is capped at [`MAX_SYSTEM_BYTES`] and an overage is a typed error
//!   naming the numbers, never a silent truncation. A persona that is quietly
//!   cut in half is worse than a turn that refuses.
//!
//! Control characters other than newline and tab are dropped, so the result is
//! always safe to place in a chat message: JSON can carry those two, and a stray
//! form feed or NUL has no business in a system prompt.

/// The hard cap on compiled system text, in bytes (32 KiB).
pub const MAX_SYSTEM_BYTES: usize = 32 * 1024;

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

impl BlockKind {
    /// The heading this block renders under. Stable, so a caller can look for it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Persona => "Persona",
            Self::ProductPolicy => "Product policy",
            Self::ToolSchemas => "Tool schemas",
            Self::SkillCatalog => "Skill catalog",
            Self::Workspace => "Workspace",
            Self::PinnedLessons => "Pinned lessons",
            Self::VolatileTail => "Volatile tail",
        }
    }
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

/// Everything a caller has to supply for one compiled prompt.
///
/// The three block kinds with no field here - tool schemas, the skill catalog
/// and pinned lessons - are carried by the request (tools) or not implemented in
/// 1.0. They stay in the order so a later wave adds a field rather than a second
/// renderer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptInputs {
    /// Who the assistant is. Rendered first, and the one block an operator edits.
    pub persona: Option<String>,
    /// Product rules the customer cannot override.
    pub product_policy: Option<String>,
    /// Workspace facts and the sandbox root.
    pub workspace: Option<String>,
    /// Anything volatile: the time, the cursor, the recent turns.
    pub volatile_tail: Option<String>,
}

impl PromptInputs {
    /// The text for one block, if this input set carries it.
    #[must_use]
    pub fn block(&self, kind: BlockKind) -> Option<&str> {
        match kind {
            BlockKind::Persona => self.persona.as_deref(),
            BlockKind::ProductPolicy => self.product_policy.as_deref(),
            BlockKind::Workspace => self.workspace.as_deref(),
            BlockKind::VolatileTail => self.volatile_tail.as_deref(),
            BlockKind::ToolSchemas | BlockKind::SkillCatalog | BlockKind::PinnedLessons => None,
        }
    }
}

/// The compiled system text for one turn.
///
/// Opaque on purpose: the only way to read it is [`CompiledPrompt::as_str`], so
/// nothing can append to it after the size check has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPrompt {
    system: String,
}

impl CompiledPrompt {
    /// The system text. Empty when no block was supplied.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.system
    }

    /// Whether this prompt carries no block at all. Callers send no system
    /// message in that case, rather than an empty one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.system.is_empty()
    }

    /// The size of the system text in bytes, which is what the cap measures.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.system.len()
    }
}

/// Why a prompt could not be compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PromptError {
    /// The compiled text is bigger than [`MAX_SYSTEM_BYTES`].
    #[error(
        "the compiled system prompt is {actual} bytes, {over} over the {limit}-byte cap; \
         shorten the persona or disable it"
    )]
    TooLarge {
        /// The cap that was exceeded.
        limit: usize,
        /// The size that was produced.
        actual: usize,
        /// `actual - limit`, named so a caller can say how much to cut.
        over: usize,
    },
}

impl PromptError {
    /// The number of bytes that have to go.
    #[must_use]
    pub const fn overage(&self) -> usize {
        match self {
            Self::TooLarge { over, .. } => *over,
        }
    }
}

/// Compile one system prompt.
///
/// Pure: the output is a byte-for-byte function of the input. Blocks appear in
/// [`DEFAULT_BLOCK_ORDER`], each as a `## <label>` section, with empty blocks
/// omitted and no trailing whitespace at either end.
///
/// # Errors
///
/// [`PromptError::TooLarge`] when the rendered text exceeds
/// [`MAX_SYSTEM_BYTES`]. The text is never truncated to fit.
pub fn compile(inputs: &PromptInputs) -> Result<CompiledPrompt, PromptError> {
    let mut system = String::new();
    for kind in DEFAULT_BLOCK_ORDER {
        let Some(raw) = inputs.block(kind) else {
            continue;
        };
        let body = clean(raw);
        if body.is_empty() {
            // Never an empty heading: a heading with nothing under it reads as
            // an instruction to invent something.
            continue;
        }
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str("## ");
        system.push_str(kind.label());
        system.push('\n');
        system.push_str(&body);
    }

    let actual = system.len();
    if actual > MAX_SYSTEM_BYTES {
        return Err(PromptError::TooLarge {
            limit: MAX_SYSTEM_BYTES,
            actual,
            over: actual - MAX_SYSTEM_BYTES,
        });
    }
    Ok(CompiledPrompt { system })
}

/// Trim a block and drop control characters other than newline and tab.
///
/// Newline and tab are the two a chat message can carry without the model
/// seeing noise; everything else in the control range is removed rather than
/// replaced, so pasting a document with form feeds cannot change the shape of
/// the prompt.
fn clean(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect();
    kept.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persona_block_is_first_in_the_compiled_prompt() {
        let inputs = PromptInputs {
            persona: Some("You are Kallisti.".to_owned()),
            workspace: Some("/home/operator/kallisti-workspace".to_owned()),
            ..PromptInputs::default()
        };
        let compiled = compile(&inputs).expect("a small prompt compiles");
        let text = compiled.as_str();
        assert!(text.starts_with("## Persona\n"), "{text}");
        let persona = text.find("## Persona").expect("the persona heading");
        let workspace = text.find("## Workspace").expect("the workspace heading");
        assert!(persona < workspace, "persona must come first: {text}");
    }

    #[test]
    fn empty_blocks_are_omitted() {
        let compiled = compile(&PromptInputs::default()).expect("an empty prompt compiles");
        assert!(compiled.is_empty(), "{}", compiled.as_str());
        assert_eq!(compiled.as_str(), "");
        assert_eq!(compiled.bytes(), 0);

        // A block that is only whitespace is empty too, and never a heading.
        let blank = PromptInputs {
            persona: Some("   \n\t\n".to_owned()),
            workspace: Some("/w".to_owned()),
            ..PromptInputs::default()
        };
        let compiled = compile(&blank).expect("compiles");
        assert!(
            !compiled.as_str().contains("## Persona"),
            "{}",
            compiled.as_str()
        );
        assert!(compiled.as_str().contains("## Workspace"));
    }

    #[test]
    fn compilation_is_deterministic() {
        let build = || PromptInputs {
            persona: Some("You are Kallisti.".to_owned()),
            product_policy: Some("Never send mail unapproved.".to_owned()),
            workspace: Some("/w".to_owned()),
            volatile_tail: Some("recent turns: none".to_owned()),
        };
        let first = compile(&build()).expect("compiles");
        let second = compile(&build()).expect("compiles");
        assert_eq!(first.as_str(), second.as_str());
        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(first, second);
        // No clock, no counter: the same inputs are the same bytes.
        assert_eq!(
            first.as_str().as_bytes(),
            second.as_str().as_bytes(),
            "byte identical"
        );
    }

    #[test]
    fn an_oversized_prompt_is_refused_with_the_overage() {
        let inputs = PromptInputs {
            persona: Some("x".repeat(MAX_SYSTEM_BYTES)),
            ..PromptInputs::default()
        };
        match compile(&inputs) {
            Err(PromptError::TooLarge {
                limit,
                actual,
                over,
            }) => {
                assert_eq!(limit, MAX_SYSTEM_BYTES);
                assert!(actual > limit, "actual {actual} must exceed {limit}");
                assert_eq!(over, actual - limit);
            }
            other => panic!("expected a size refusal, got {other:?}"),
        }
        let error = compile(&inputs).expect_err("must refuse");
        let rendered = error.to_string();
        assert!(
            rendered.contains(&MAX_SYSTEM_BYTES.to_string()),
            "{rendered}"
        );
        assert!(
            rendered.contains("over the"),
            "the overage is named: {rendered}"
        );
    }

    #[test]
    fn a_missing_persona_compiles_to_no_system_block() {
        let inputs = PromptInputs {
            workspace: Some("/w".to_owned()),
            ..PromptInputs::default()
        };
        let compiled = compile(&inputs).expect("compiles without a persona");
        assert!(!compiled.as_str().contains("## Persona"));
        assert!(compiled.as_str().contains("## Workspace"));
    }

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
