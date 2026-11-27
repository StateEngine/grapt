//! Shared presentation styles, said as meaning rather than as colour.
//!
//! ## Why a view names a role and not a colour
//!
//! A view says "this is an error" or "this line was added"; the theme says
//! what an error looks like. A view that picked colours would make every theme
//! and every `NO_COLOR` terminal a view change, and a snapshot of it would be
//! a palette snapshot. So the common case is a [`Token`], resolved only by the
//! sink that writes bytes. [`Paint::Indexed`] and [`Paint::Rgb`] exist for
//! content that arrives already coloured — a syntax theme's palette index or
//! its true colour — which a role cannot express without losing it.
//!
//! Nothing here touches a terminal. Styles stay pure values.

use std::ops::BitOr;

/// A colour role. The theme decides what it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Text,
    Muted,
    Accent,
    User,
    Thinking,
    Tool,
    Error,
    Warning,
    Success,
    DiffAdd,
    DiffDel,
    DiffContext,
    Link,
    Code,
    CodeBlock,
    Heading,
    Quote,
    Border,
    Selection,
    Status,
}

/// A foreground or background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Paint {
    /// The terminal's own colour.
    #[default]
    Default,
    /// An entry of the terminal's palette, kept as an index so the user's
    /// palette still applies.
    Indexed(u8),
    /// A role the theme resolves.
    Token(Token),
    /// A true colour, approximated where the terminal has none.
    Rgb(u8, u8, u8),
}

/// Text attributes, combined with `|`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Emphasis(u8);

impl Emphasis {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const DIM: Self = Self(1 << 3);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Emphasis {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

/// How one stretch of a row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Style {
    pub fg: Paint,
    pub bg: Paint,
    pub emphasis: Emphasis,
}

impl Style {
    /// The terminal's defaults: what an unstyled row has always been.
    pub const PLAIN: Self = Self {
        fg: Paint::Default,
        bg: Paint::Default,
        emphasis: Emphasis::NONE,
    };

    /// Foreground in a role.
    pub const fn token(token: Token) -> Self {
        Self {
            fg: Paint::Token(token),
            ..Self::PLAIN
        }
    }

    pub const fn on(self, bg: Paint) -> Self {
        Self { bg, ..self }
    }

    pub const fn with(self, emphasis: Emphasis) -> Self {
        Self {
            emphasis: self.emphasis.union(emphasis),
            ..self
        }
    }

    pub fn is_plain(&self) -> bool {
        *self == Self::PLAIN
    }
}

#[cfg(test)]
mod tests;
