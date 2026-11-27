use super::*;

#[tokio::test]
async fn failed_empty_body_is_unavailable_but_successful_empty_eof_is_captured() {
    for failed in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("fixture bind: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("{error}"));
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            let mut request = Vec::new();
            for _ in 0..16 {
                let mut chunk = [0u8; 4096];
                let count = socket
                    .read(&mut chunk)
                    .await
                    .unwrap_or_else(|error| panic!("{error}"));
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|held| held == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(request.windows(4).any(|held| held == b"\r\n\r\n"));
            let length = usize::from(failed);
            socket.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").as_bytes())
                .await.unwrap_or_else(|error| panic!("{error}"));
        });
        let adapter = adapter(1).with_error_text(
            crate::ProviderErrorText::at("/profile/logs/provider-error.txt")
                .unwrap_or_else(|error| panic!("{error}")),
        );
        let opened = adapter
            .open(input(
                "att_body_failure",
                &format!("http://{address}"),
                ProtocolFamily::OpenAiCompatible,
                compat_request(),
            ))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let (fragment, detail) = match opened {
            ProviderOpen::FailedWithDetail { fragment, detail } => (fragment, detail),
            _ => panic!("missing provider refusal/detail"),
        };
        assert_eq!(detail.status(), 400);
        assert_eq!(detail.text().is_none(), failed);
        if !failed {
            assert_eq!(
                detail
                    .text()
                    .unwrap_or_else(|| panic!("missing empty body"))
                    .as_str(),
                ""
            );
        }
        assert!(matches!(
            *fragment,
            AttemptFragment::ProviderFailure {
                class: crate::ProviderFailureClass::InvalidRequest,
                classification: None,
                ..
            }
        ));
        assert_eq!(adapter.active_len(), 0);
        server
            .await
            .unwrap_or_else(|error| panic!("fixture server: {error}"));
    }
}

#[tokio::test]
async fn capture_preserves_status_overflow_and_request_bytes_with_and_without_opt_in() {
    for status in [400, 401, 413, 422, 429, 500] {
        for body in [
            "{\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"token-value-never-printed sk-private-echo\"}}".to_owned(),
            "plain text error token-value-never-printed".to_owned(),
            "{malformed sk-private-echo".to_owned(),
            format!("{{\"error\":{{\"code\":\"context_length_exceeded\",\"message\":\"{}\"}}}}", "x".repeat(17 * 1024)),
        ] {
            let mut outcomes = Vec::new();
            let mut requests = Vec::new();
            for enabled in [false, true] {
                let fixture = serve(status, &[("retry-after", "3")], vec![body.as_bytes().to_vec()]).await;
                let policy = crate::ProviderErrorText::at("/profile/logs/provider-error.txt")
                    .unwrap_or_else(|e| panic!("{e}"));
                let adapter = adapter(1).with_error_text(if enabled { policy } else { crate::ProviderErrorText::disabled() });
                let opened = adapter.open(input("att_error_text", &fixture.base,
                    ProtocolFamily::OpenAiCompatible, compat_request())).await
                    .unwrap_or_else(|e| panic!("{e}"));
                let (opened, detail) = match opened {
                    ProviderOpen::FailedWithDetail { fragment, detail } => (ProviderOpen::Failed(fragment), Some(detail)),
                    other => (other, None),
                };
                let fragments = drain(opened).await;
                let (class, classification, message) = fragments.iter().find_map(|held| match held {
                    AttemptFragment::ProviderFailure { class, classification, message, .. } => Some((class.clone(), *classification, message)),
                    _ => None,
                }).unwrap_or_else(|| panic!("missing refusal"));
                assert!(!message.contains("token-value") && !message.contains("sk-private"));
                // Every failed status shows a masked excerpt, opted in or not,
                // and no row advertises the opt-in file. The vendor's machine
                // code is named when the body published one.
                let reason = crate::transport::error_text::mask(body.as_bytes(), "token-value-never-printed");
                // The vendor's code comes from the same parsed body the adapter
                // reads: only a complete client-error body inside the byte cap
                // is parsed, so an oversized or server-error body names none.
                let code = (status < 500 && body.len() <= crate::transport::classify::MAX_ERROR_BYTES)
                    .then(|| serde_json::from_str::<serde_json::Value>(&body).ok())
                    .flatten()
                    .and_then(|body| body.get("error").cloned())
                    .and_then(|error| error.get("code").cloned())
                    .and_then(|code| crate::VendorCode::from_json(&code));
                let expected = crate::transport::error_text::refusal_message(
                    status,
                    code,
                    Some(&reason),
                );
                assert_eq!(message, &expected, "status {status}");
                assert!(!message.contains("provider-error.txt"), "{message}");
                let captured = detail.as_ref().and_then(crate::ProviderErrorDetail::text);
                // The opt-in file remains a client-error path only: a 5xx is
                // shown in the row but never written to the file. This is the
                // pre-existing file contract, unchanged by the display ruling.
                if enabled && status < 500 {
                    let captured = captured.unwrap_or_else(|| panic!("missing capture for {status}"));
                    assert!(captured.as_str().len() <= 512);
                    assert!(!captured.as_str().contains("token-value") && !captured.as_str().contains("sk-private"));
                } else {
                    assert!(captured.is_none(), "unexpected capture for {status}");
                }
                outcomes.push((class, classification));
                requests.push(fixture.body.lock().unwrap_or_else(|e| panic!("{e}")).clone());
                assert_eq!(adapter.active_len(), 0);
            }
            assert_eq!(outcomes[0], outcomes[1], "opt-in changed classification for {status}");
            assert_eq!(requests[0], requests[1], "opt-in changed outgoing request bytes");
        }
    }
}

#[tokio::test]
async fn a_delayed_body_keeps_status_classification_and_reports_unavailable_capture() {
    for enabled in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("fixture bind: {e}"));
        let address = listener.local_addr().unwrap_or_else(|e| panic!("{e}"));
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap_or_else(|e| panic!("{e}"));
            let mut request = Vec::new();
            for _ in 0..16 {
                let mut chunk = [0u8; 4096];
                let count = socket
                    .read(&mut chunk)
                    .await
                    .unwrap_or_else(|e| panic!("{e}"));
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|held| held == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(
                request.windows(4).any(|held| held == b"\r\n\r\n"),
                "missing request head"
            );
            socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 8\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            // Keep the body absent until the client has settled. A timed
            // server exit would turn a delayed read into an unrelated EOF.
            std::future::pending::<()>().await;
        });
        // Delay the body, not the response head: use the ordinary fixture
        // deadlines so a busy executor can still receive the 400 status.
        // The absent body expires at the adapter's five-second capture cap.
        let deadlines = deadlines();
        let policy = crate::ProviderErrorText::at("/profile/logs/provider-error.txt")
            .unwrap_or_else(|e| panic!("{e}"));
        let adapter = ProviderAdapter::new(
            crate::transport::client::build_loopback_client,
            deadlines,
            1,
        )
        .with_error_text(if enabled {
            policy
        } else {
            crate::ProviderErrorText::disabled()
        });
        // Bound the whole open beyond both the head and error-body reads.
        // The server stays open until the adapter has settled.
        let opened = tokio::time::timeout(
            Duration::from_secs(30),
            adapter.open(input(
                "att_delayed_error",
                &format!("http://{address}"),
                ProtocolFamily::OpenAiCompatible,
                compat_request(),
            )),
        )
        .await;
        // Release and join the exact fixture task before inspecting outcomes,
        // including a refused open or watchdog expiry.
        server.abort();
        let stopped = server.await;
        assert!(matches!(stopped, Err(error) if error.is_cancelled()));
        let opened = opened
            .unwrap_or_else(|_| panic!("error read deadline did not settle"))
            .unwrap_or_else(|e| panic!("{e}"));
        let (opened, detail) = match opened {
            ProviderOpen::FailedWithDetail { fragment, detail } => {
                (ProviderOpen::Failed(fragment), Some(detail))
            }
            other => (other, None),
        };
        let fragments = drain(opened).await;
        assert_eq!(
            failure_of(&fragments),
            Some(crate::ProviderFailureClass::InvalidRequest)
        );
        let message = fragments
            .iter()
            .find_map(|held| match held {
                AttemptFragment::ProviderFailure {
                    message,
                    classification,
                    ..
                } => {
                    assert_eq!(*classification, None);
                    Some(message)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing failure"));
        assert!(message.contains("HTTP 400"));
        // The row no longer advertises the opt-in file; the excerpt is all it
        // carries, and an unreadable body leaves the status alone.
        assert!(!message.contains("provider-error.txt"), "{message}");
        assert_eq!(message, "the provider returned HTTP 400");
        // A body that never arrived is absent, not empty: no excerpt is
        // captured, and the detail still states whether the opt-in was on.
        assert!(detail
            .as_ref()
            .and_then(crate::ProviderErrorDetail::text)
            .is_none());
        assert_eq!(
            detail.as_ref().map(crate::ProviderErrorDetail::enabled),
            Some(enabled),
            "the detail carries the opt-in state beside the failure"
        );
        assert_eq!(adapter.active_len(), 0);
    }
}
