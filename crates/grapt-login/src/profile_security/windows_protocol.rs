//! Bounded, correlated frames for the fixed managed protection helper.
use std::io::{self, Read};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const OUTPUT_BYTES: usize = 128 * 1024;

#[derive(Serialize)]
struct Request<'a> {
    request_id: u32,
    operation: &'a str,
    data: &'a str,
}

pub(super) fn request(id: u32, operation: &str, data: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    if data.len() > 64 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile protection input exceeds its byte bound",
        ));
    }
    let encoded = Zeroizing::new(STANDARD.encode(data));
    let mut frame = Zeroizing::new(serde_json::to_vec(&Request {
        request_id: id,
        operation,
        data: &encoded,
    })?);
    frame.push(b'\n');
    Ok(frame)
}

pub(super) fn read(output: &mut impl Read) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut frame = Zeroizing::new(Vec::new());
    let mut buffer = Zeroizing::new([0_u8; 4096]);
    loop {
        let count = output.read(&mut buffer[..])?;
        if count == 0 || frame.len().saturating_add(count) > OUTPUT_BYTES {
            return Err(io::Error::other(
                "profile protection frame is incomplete or oversized",
            ));
        }
        frame.extend_from_slice(&buffer[..count]);
        if let Some(index) = buffer[..count].iter().position(|byte| *byte == b'\n') {
            if index + 1 != count {
                return Err(io::Error::other(
                    "profile protection returned multiple frames",
                ));
            }
            return Ok(frame);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    request_id: u32,
    data: Option<String>,
    error: Option<String>,
    windows: Option<super::refusal::WindowsFailure>,
}

pub(super) fn response(id: u32, frame: &[u8]) -> io::Result<Vec<u8>> {
    let reply: Response = serde_json::from_slice(frame)
        .map_err(|_| io::Error::other("invalid profile protection frame"))?;
    let data = reply.data.map(Zeroizing::new);
    if reply.request_id != id || data.is_some() == reply.error.is_some() {
        return Err(io::Error::other(
            "profile protection response does not match the request",
        ));
    }
    if let Some(error) = reply.error {
        return Err(super::refusal::windows_refusal(
            &error,
            reply.windows.unwrap_or_default(),
        ));
    }
    if reply.windows.is_some() {
        return Err(io::Error::other("unexpected Windows protection diagnostic"));
    }
    STANDARD
        .decode(
            data.as_deref()
                .ok_or_else(|| io::Error::other("profile protection result is absent"))?,
        )
        .map_err(|_| io::Error::other("invalid Windows protection output"))
}

#[cfg(test)]
mod tests {
    use super::super::{failure_code, failure_diagnostic};
    use super::*;
    use serde_json::json;

    #[test]
    fn windows_refusal_preserves_operation_code_and_profile_state() -> io::Result<()> {
        for operation in ["seal", "open", "acl-write"] {
            for (source, label) in [("native", "native"), ("hresult", "HRESULT")] {
                for (profile, label_profile) in [
                    (Some(true), "loaded"),
                    (Some(false), "unloaded"),
                    (None, "unavailable"),
                ] {
                    let frame = serde_json::to_vec(&json!({
                        "request_id":3,"error":operation,
                        "windows":{"win32_code":5,"code_source":source,"profile_loaded":profile}
                    }))?;
                    let error = response(3, &frame)
                        .err()
                        .ok_or_else(|| io::Error::other("refusal became success"))?;
                    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                    assert_eq!(failure_code(&error), operation);
                    let detail = failure_diagnostic(&error);
                    assert!(detail.contains(&format!("operation={operation}")));
                    assert!(detail.contains(&format!("win32=5 ({label})")));
                    assert!(detail.contains(&format!("user-profile-hive={label_profile}")));
                }
            }
        }
        let legacy = response(3, br#"{"request_id":3,"error":"open"}"#)
            .err()
            .ok_or_else(|| io::Error::other("legacy refusal became success"))?;
        assert_eq!(failure_code(&legacy), "open");
        let detail = failure_diagnostic(&legacy);
        assert!(detail.contains("win32=unavailable"));
        assert!(detail.contains("user-profile-hive=unavailable"));
        let frame = serde_json::to_vec(&json!({
            "request_id":3,"error":"seal",
            "windows":{"win32_code":null,"code_source":null,"profile_loaded":false}
        }))?;
        let error = response(3, &frame)
            .err()
            .ok_or_else(|| io::Error::other("unavailable code became success"))?;
        assert_eq!(failure_code(&error), "seal");
        let detail = failure_diagnostic(&error);
        assert!(detail.contains("win32=unavailable"));
        assert!(detail.contains("user-profile-hive=unloaded"));
        Ok(())
    }

    #[test]
    fn windows_refusal_rejects_untrusted_diagnostic_fields() -> io::Result<()> {
        for windows in [
            json!({"win32_code":5,"code_source":"native","profile_loaded":true,"exception":"SYNTHETIC_PRIVATE"}),
            json!({"win32_code":5,"code_source":"exception-text"}),
            json!({"win32_code":5,"code_source":null}),
            json!({"win32_code":null,"code_source":"native"}),
            json!({"win32_code":-1,"code_source":"native"}),
            json!({"win32_code":65536,"code_source":"native"}),
            json!({"win32_code":1.5,"code_source":"hresult"}),
            json!({"profile_loaded":"SYNTHETIC_PRIVATE"}),
        ] {
            let frame =
                serde_json::to_vec(&json!({"request_id":3,"error":"seal","windows":windows}))?;
            let error = response(3, &frame)
                .err()
                .ok_or_else(|| io::Error::other("invalid diagnostic accepted"))?;
            assert_eq!(failure_code(&error), "io");
            assert!(!error.to_string().contains("SYNTHETIC_PRIVATE"));
        }
        for frame in [
            json!({"request_id":3,"data":"","windows":{}}),
            json!({"request_id":3,"error":"seal","exception":"SYNTHETIC_PRIVATE"}),
        ] {
            assert!(response(3, &serde_json::to_vec(&frame)?).is_err());
        }
        Ok(())
    }

    #[test]
    fn protection_diagnostics_never_include_raw_error_text() -> io::Result<()> {
        const PRIVATE: &str = "C:/private/account SYNTHETIC_CREDENTIAL_NOT_VALID";
        let ordinary = io::Error::new(io::ErrorKind::PermissionDenied, PRIVATE);
        assert_eq!(failure_code(&ordinary), "io");
        assert!(!failure_diagnostic(&ordinary).contains(PRIVATE));
        for operation in [PRIVATE, "acl-read\nSYNTHETIC_CREDENTIAL_NOT_VALID"] {
            let frame = serde_json::to_vec(&json!({
                "request_id":3,"error":operation,
                "windows":{"win32_code":5,"code_source":"native","profile_loaded":false}
            }))?;
            let error = response(3, &frame)
                .err()
                .ok_or_else(|| io::Error::other("untrusted refusal became success"))?;
            assert_eq!(failure_code(&error), "child-failed");
            for text in [
                error.to_string(),
                format!("{error:?}"),
                failure_diagnostic(&error),
            ] {
                assert!(!text.contains("SYNTHETIC_CREDENTIAL_NOT_VALID"));
                assert!(!text.contains("C:/private/account"));
            }
        }
        Ok(())
    }

    #[test]
    fn only_the_complete_matching_reply_can_supply_a_protection_result() -> io::Result<()> {
        let request = request(3, "open", b"private")?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request)?["request_id"],
            3
        );
        let good = b"{\"request_id\":3,\"data\":\"cHJpdmF0ZQ==\"}\r\n";
        assert_eq!(response(3, &read(&mut &good[..])?)?, b"private");
        assert!(response(4, good).is_err());
        assert!(read(&mut &good[..good.len() - 2]).is_err());
        assert!(read(&mut &b"{}\n{}\n"[..]).is_err());
        assert!(read(&mut std::io::repeat(b'x').take(OUTPUT_BYTES as u64 + 1)).is_err());
        assert!(response(3, b"{\"request_id\":3,\"data\":\"\",\"error\":\"owner\"}").is_err());
        let refusal = response(3, b"{\"request_id\":3,\"error\":\"foreign-write\"}")
            .err()
            .ok_or_else(|| io::Error::other("missing protection refusal"))?;
        assert_eq!(super::super::failure_code(&refusal), "foreign-write");
        Ok(())
    }
}
