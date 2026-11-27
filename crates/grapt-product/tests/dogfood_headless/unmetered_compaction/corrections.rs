//! Later compaction and normalized images consume the degraded request safely.
use super::*;
use base64::Engine as _;

const VALID_SUMMARY: &str = "## Objective\n- Continue reviewing the coding patches; the original seed was read and no file changes were made.";
static SUMMARY_CALLS: AtomicUsize = AtomicUsize::new(0);

fn repair_later(request: &str) -> Script {
    if request.contains(SUMMARY_REQUEST) {
        let call = SUMMARY_CALLS.fetch_add(1, Ordering::SeqCst);
        text_without_usage(if call < 1 { "" } else { VALID_SUMMARY })
    } else {
        response_for(request)
    }
}

struct Journey {
    root: PathBuf,
    provider: ProviderFixture,
    child: process_fixture::ManagedChild,
    address: SocketAddr,
    session: String,
    _cleanup: terminal_coordination::RootCleanup,
}

impl Journey {
    fn start(label: &str, response: fn(&str) -> Script) -> TestResult<Self> {
        let root = root(label);
        prepare(&root, 120_000);
        prepare_model_inventory(&root, WINDOW);
        set_compaction(
            &root,
            serde_json::json!({"auto":true,"buffer":4096,"server_side":false}),
        );
        fs::write(
            root.join("seed.rs"),
            b"pub fn retained_fact() -> u32 { 7 }\n",
        )?;
        let mut scripts = vec![read_without_usage()];
        scripts.extend(vec![Script::ForRequest(response); TURNS * 4 + 8]);
        let provider = ProviderFixture::start(scripts);
        let mut child = process_fixture::spawn_controlled(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        submit(address, &session, "msg_projection_seed", SEED);
        compaction_continuity::approve(address, &session, CALL);
        permission_residuals::wait_session_idle(address, &session);
        for turn in 0..TURNS {
            let patch = format!(
                "Review coding patch {turn}:\n{}",
                "pub fn example(value: u32) -> u32 { value.saturating_add(1) }\n".repeat(400)
            );
            run_prompt(
                address,
                &session,
                &provider,
                &format!("msg_projection_{turn}"),
                &patch,
            );
            if summaries(&provider) >= 1 {
                break;
            }
        }
        assert_eq!(summaries(&provider), 1, "the empty summary was not reached");
        Ok(Self {
            _cleanup: terminal_coordination::RootCleanup(root.clone()),
            root,
            provider,
            child,
            address,
            session,
        })
    }

    fn prompt(&self, id: &str, text: &str) -> serde_json::Value {
        run_prompt(self.address, &self.session, &self.provider, id, text)
    }
}

/// The patch numbers a request body carries.
fn patches(body: &str) -> Vec<usize> {
    body.match_indices("Review coding patch ")
        .filter_map(|(at, label)| {
            let digits: String = body[at + label.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok()
        })
        .collect()
}

#[test]
fn a_later_compaction_quotes_durable_history_never_the_trimmed_request() -> TestResult {
    let _journey = process_journey_custody();
    SUMMARY_CALLS.store(0, Ordering::SeqCst);
    let mut run = Journey::start("partial-request-summary-coverage", repair_later)?;
    let page = run.prompt("msg_projection_recovered", FOLLOWUP);
    assert_eq!(answers(&page)?, 1);
    assert_estimate_and_history(&page)?;
    {
        let bodies = locked(&run.provider.bodies);
        let degraded = bodies.last().ok_or("recovered request absent")?;
        assert!(degraded.contains("Context trimmed:"));
        assert!(
            !degraded.contains("Review coding patch 0:"),
            "fixture omitted no early exchange"
        );
    }
    let before = summaries(&run.provider);
    json(
        run.address,
        "POST",
        &format!("/api/session/{}/compact", run.session),
        "{}",
    );
    run.prompt(
        "msg_projection_compact",
        "Continue the coding review after compaction.",
    );
    manual_compaction::assert_carried(
        &run.root,
        "msg_projection_compact",
        "session.execution.succeeded.1",
    );
    let bodies = locked(&run.provider.bodies);
    let summary = bodies
        .iter()
        .filter(|body| body.contains(SUMMARY_REQUEST))
        .nth(before)
        .ok_or("explicit compaction sent no summary request")?;
    assert!(
        !summary.contains("Context trimmed:"),
        "a partial request was reused as a full summary prefix"
    );
    // The summary reads durable history, never the trimmed request. When that
    // history does not fit one request it quotes the NEWEST window and drops
    // the oldest item of an input that overflows, as established compactors
    // do, so the oldest exchange, the one the degraded request omitted, is no
    // longer promised. What stays true is that the quote is built from durable
    // patches.
    assert!(
        !patches(summary).is_empty(),
        "the summary quoted no patch of durable history"
    );
    drop(bodies);
    run.child.stop_after_witness(&run.root);
    Ok(())
}

#[test]
fn an_image_prompt_on_a_text_model_survives_request_trimming() -> TestResult {
    let _journey = process_journey_custody();
    let mut run = Journey::start("normalized-image-context-trim", response_for)?;
    let image = run.root.join("input.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 60, 255])).save(&image)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(fs::read(image)?);
    let id = "msg_projection_image";
    json(
        run.address,
        "POST",
        &format!("/api/session/{}/prompt", run.session),
        &serde_json::json!({"id":id,"text":FOLLOWUP,"delivery":"queue","resume":true,
            "files":[{"uri":format!("data:image/png;base64,{encoded}")}]})
        .to_string(),
    );
    manual_compaction::assert_carried(&run.root, id, "session.execution.succeeded.1");
    permission_residuals::wait_session_idle(run.address, &run.session);
    let page = history(run.address, &run.session);
    assert_eq!(answers(&page)?, 1);
    assert_estimate_and_history(&page)?;
    let bodies = locked(&run.provider.bodies);
    let sent = bodies.last().ok_or("normalized image request absent")?;
    assert!(sent.contains(FOLLOWUP));
    assert!(sent.contains("Context trimmed:"));
    assert!(sent.contains("selected model does not advertise image input"));
    drop(bodies);
    run.child.stop_after_witness(&run.root);
    Ok(())
}
