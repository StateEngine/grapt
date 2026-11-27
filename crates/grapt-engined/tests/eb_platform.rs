//! EB03 — an unsupported target is refused by name, never down a Unix arm.
//!
//! The module is declared by path rather than through a library target: this
//! crate is a binary composition root and deliberately has no `lib.rs`, and a
//! `#[cfg(test)]` unit inside `main.rs` would witness a law from inside the
//! artifact it constrains. `#[path]` is an ordinary named module declaration,
//! not an `include!` fragment, so it adds no direct-include site and a mutant
//! applied to `src/platform.rs` still reddens this witness.

// The module is shared source compiled into this test binary as well as into
// the binary itself. Each witness file uses the part of it that carries its own
// law, so items belonging to a sibling witness are legitimately unused HERE.
#[allow(dead_code)]
#[path = "../src/platform.rs"]
mod platform;

use platform::{platform_support, PlatformSupport};

/// The 2026-10-19 parity decision explicitly admits macOS.
#[test]
fn macos_is_supported_without_refusal() {
    assert_eq!(platform_support("macos"), PlatformSupport::Supported);
    assert!(platform_support("macos").refusal_text().is_none());
}

/// A Unix family or a near-match must never admit an unlisted operating system.
#[test]
fn unsupported_operating_systems_are_refused_by_exact_name() {
    for other in [
        "freebsd", "android", "ios", "netbsd", "solaris", "unix", "darwin", "MacOS", "Linux",
        "windows ", "",
    ] {
        let verdict = platform_support(other);
        assert_eq!(
            verdict,
            PlatformSupport::Unsupported {
                target_os: other.to_owned()
            },
            "{other:?} is not an exact supported operating-system name"
        );
        let text = verdict
            .refusal_text()
            .unwrap_or_else(|| panic!("an unsupported platform must carry a refusal text"));
        assert!(
            text.contains(&format!("target_os={other};")),
            "the refusal must name the exact platform: {text:?}"
        );
    }
}

/// The direction guard keeps existing supported platforms admitted.
#[test]
fn windows_and_linux_remain_supported_without_refusal() {
    for name in ["windows", "linux"] {
        assert_eq!(platform_support(name), PlatformSupport::Supported);
        assert!(platform_support(name).refusal_text().is_none());
    }
}
