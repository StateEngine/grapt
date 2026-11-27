// Copyright 2025 OpenAI.
// SPDX-License-Identifier: Apache-2.0
//! Display-only Windows device path simplification. Canonical paths and
//! persisted tool results stay intact.
use std::borrow::Cow;

pub(super) fn failure_text(text: &str) -> Cow<'_, str> {
    if !text.contains(r"\\?\") {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace(r"\\?\UNC\", r"\\").replace(r"\\?\", ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_paths_are_simplified_only_in_the_display_copy() {
        let original = r"outside \\?\C:\work; remote \\?\UNC\server\share\file";
        assert_eq!(
            failure_text(original),
            r"outside C:\work; remote \\server\share\file"
        );
        assert!(original.contains(r"\\?\"));
        assert!(matches!(
            failure_text(r"C:\work /mnt/work"),
            Cow::Borrowed(_)
        ));
    }
}
