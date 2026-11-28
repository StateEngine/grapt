use super::*;
use std::io::{self, ErrorKind};

#[test]
fn native_causes_keep_the_operation_kind_and_numeric_code() {
    for code in [5, 13, 32, i32::MAX] {
        let cause = io::Error::from_raw_os_error(code);
        let failure = StorageFailure::capture(StorageOperation::OpenLock, &cause);
        assert_eq!(failure.operation, StorageOperation::OpenLock);
        assert_eq!(failure.kind, cause.kind());
        assert_eq!(failure.os_code, Some(code));
        assert_eq!(failure.protection, "io");
        let error = VaultError::Unavailable(failure);
        assert!(error.to_string().contains("OpenLock"));
        assert!(error.to_string().contains(&format!("os={code}")));
        assert!(!error.to_string().contains("protection="));
        assert!(std::error::Error::source(&error).is_none());
    }
}

#[test]
fn arbitrary_error_payloads_do_not_survive_safe_capture() {
    let sentinel = "synthetic-secret-unsafe\n/private/error-path\t\u{1b}";
    let cause = io::Error::new(ErrorKind::PermissionDenied, sentinel);
    let failure = StorageFailure::capture(StorageOperation::WriteEntry, &cause);
    assert_eq!(failure.kind, ErrorKind::PermissionDenied);
    assert_eq!(failure.os_code, None);
    assert_eq!(failure.protection, "io");
    drop(cause);
    let error = VaultError::Unavailable(failure);
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(rendered.contains("PermissionDenied"));
        assert!(rendered.contains("WriteEntry"));
        for private in ["synthetic-secret", "/private/error-path"] {
            assert!(!rendered.contains(private));
        }
        assert!(!rendered.chars().any(char::is_control));
        assert!(rendered.len() < 256);
    }
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn managed_refusals_retain_only_the_existing_fixed_code() {
    for (output, code) in [
        (b"error:acl-read".as_slice(), "acl-read"),
        (b"error:foreign-write".as_slice(), "foreign-write"),
        (b"error:seal".as_slice(), "seal"),
        (b"error:open".as_slice(), "open"),
        (
            b"error:synthetic-secret\n/private/path".as_slice(),
            "child-failed",
        ),
    ] {
        let cause = crate::profile_security::child_refusal(output);
        let failure = StorageFailure::capture(StorageOperation::ProtectLock, &cause);
        assert_eq!(failure.kind, ErrorKind::PermissionDenied);
        assert_eq!(failure.os_code, None);
        assert_eq!(failure.protection, code);
        let error = VaultError::Unavailable(failure);
        assert!(error.to_string().contains("ProtectLock"));
        assert!(error.to_string().contains(&format!("protection={code}")));
        assert!(!error.to_string().contains("os="));
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("synthetic-secret"));
            assert!(!rendered.contains("/private/path"));
        }
    }
}

#[test]
fn a_refused_session_bus_keeps_its_repair_step_at_both_seams() {
    use crate::profile_security::{bus_refused, failure_diagnostic, BusRefusal};
    for bus in [BusRefusal::Malformed, BusRefusal::Unreachable] {
        let diagnostic = failure_diagnostic(&bus_refused(bus));
        assert!(diagnostic.contains(bus.guidance()), "{diagnostic}");
        let refusal = sealed_file::unavailable(StorageOperation::Seal)(bus_refused(bus));
        assert!(
            matches!(refusal, VaultError::KeystoreBus(kind) if kind == bus),
            "{refusal:?}"
        );
        assert!(refusal.to_string().contains("unset it"), "{refusal}");
    }
}
