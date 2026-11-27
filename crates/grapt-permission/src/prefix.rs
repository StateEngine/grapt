//! Exact command-argument prefixes for configured execution rules.
//!
//! (Apache-2.0, Copyright 2025 OpenAI).
//! Modified: bounded construction and borrowed boolean matching; parsing shell
//! syntax remains the product's existing command classifier's responsibility.

const MAX_TOKENS: usize = 64;
const MAX_ALTERNATIVES: usize = 64;
const MAX_BYTES: usize = 4096;

/// The program is literal; each subsequent argument is a literal or a set of
/// alternatives. Neither globbing nor executable basename rewriting applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixPattern {
    first: String,
    rest: Vec<Vec<String>>,
}

impl PrefixPattern {
    /// Construct an argv prefix. Each position supplies one or more exact
    /// alternatives; the program position must contain exactly one nonempty name.
    pub fn new(mut positions: Vec<Vec<String>>) -> Result<Self, &'static str> {
        if positions.is_empty() || positions.len() > MAX_TOKENS {
            return Err("execution prefix must contain 1..64 argument positions");
        }
        if positions[0].len() != 1 || positions[0][0].is_empty() {
            return Err("execution prefix must start with one literal program");
        }
        let mut bytes = 0usize;
        for alternatives in &positions {
            if alternatives.is_empty() || alternatives.len() > MAX_ALTERNATIVES {
                return Err("execution prefix position must contain 1..64 alternatives");
            }
            for token in alternatives {
                if token.contains('\0') || token.len() > MAX_BYTES.saturating_sub(bytes) {
                    return Err("execution prefix must contain at most 4096 bytes and no NUL");
                }
                bytes += token.len();
            }
        }
        let mut program = positions.remove(0);
        Ok(Self {
            first: program.remove(0),
            rest: positions,
        })
    }

    /// Match a previously parsed simple command. Callers must decide every
    /// command in a compound expression rather than treating its first command
    /// as authorization for the expression.
    pub fn matches(&self, command: &[String]) -> bool {
        let pattern_length = self.rest.len() + 1;
        if command.len() < pattern_length || command[0] != self.first {
            return false;
        }
        self.rest
            .iter()
            .zip(&command[1..pattern_length])
            .all(|(alternatives, token)| alternatives.contains(token))
    }
}

#[cfg(test)]
#[path = "prefix/tests.rs"]
mod tests;
