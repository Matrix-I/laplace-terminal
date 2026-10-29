//! Shell prompt mark tracking for OSC 133 semantic prompt jump and output selection.

/// Prompts remembered for jump-to-prompt and output selection.
pub const MAX_PROMPTS: usize = 1000;

/// One shell prompt from OSC 133 marks, as absolute lines.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PromptMark {
    /// Line of the prompt (OSC 133;A).
    pub prompt: usize,
    /// First output line (OSC 133;C) and the line after the last (OSC 133;D).
    pub output: Option<(usize, Option<usize>)>,
}
