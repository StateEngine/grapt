use super::*;
use grapt_provider::CodexToolText;
use grapt_types::ToolCallId;

/// Tool outputs of at most `bytes` UTF-8 bytes, cut at a character boundary,
/// that escape heavily in JSON and hold a multibyte character, so no
/// allocation can rely on one encoded byte per output byte.
fn outputs(count: usize, bytes: usize) -> Result<Vec<(u64, CodexInputItem)>, String> {
    (0..count)
        .map(|index| {
            let unit = "line \"quoted\"\n\u{e9}\t";
            let mut text = unit.repeat(bytes / unit.len() + 1);
            let mut end = bytes;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            Ok((
                u64::try_from(index).map_err(|error| error.to_string())? + 1,
                CodexInputItem::ToolOutput {
                    call: ToolCallId::new(format!("call_payload_{index}")).ok_or("call")?,
                    output: CodexToolText::try_new(text).map_err(|error| format!("{error:?}"))?,
                    is_error: false,
                },
            ))
        })
        .collect()
}

fn session() -> Result<SessionId, String> {
    "ses_compact01_payload"
        .parse()
        .map_err(|_| "session id".to_owned())
}

/// Encodes `rows` within `available` bytes as `summary_payload` does.
fn allocate(
    rows: &[(u64, CodexInputItem)],
    available: usize,
) -> Result<(String, Vec<usize>), String> {
    let session = session()?;
    let rows: Vec<_> = rows.iter().map(|(seq, item)| (*seq, item)).collect();
    let mut quoted = vec![0; rows.len()];
    let baseline = encode_summary_payload(None, &session, "continue", &rows, &quoted)?;
    allocate_outputs(&session, &rows, &mut quoted, available - baseline.len())?;
    Ok((
        encode_summary_payload(None, &session, "continue", &rows, &quoted)?,
        quoted,
    ))
}

/// SC-COMPACT-01: the summariser was given a few hundred bytes of each tool
/// output because the spare was divided by six times the output count. The
/// allocation now measures each grant, so the payload fills its budget,
/// never exceeds it, and the newest outputs are quoted furthest.
#[test]
fn a_summary_payload_fills_its_budget_newest_outputs_first() -> Result<(), String> {
    let rows = outputs(40, 20_000)?;
    let session = session()?;
    let refs: Vec<_> = rows.iter().map(|(seq, item)| (*seq, item)).collect();
    let baseline = encode_summary_payload(None, &session, "continue", &refs, &[0; 40])?.len();
    let available = baseline + 200_000;
    let (payload, quoted) = allocate(&rows, available)?;
    assert!(
        payload.len() <= available,
        "{} > {available}",
        payload.len()
    );
    assert!(
        payload.len() + OUTPUT_SEARCH_MIN >= available,
        "{} of {available} bytes used",
        payload.len()
    );
    assert_eq!(
        quoted[39], OUTPUT_CEILING,
        "the newest output is not quoted furthest"
    );
    assert!(
        quoted.iter().all(|bytes| *bytes >= OUTPUT_FLOOR),
        "{quoted:?}"
    );
    assert!(
        quoted.windows(2).all(|pair| pair[0] <= pair[1]),
        "an older output was quoted further than a newer one: {quoted:?}"
    );
    Ok(())
}

#[test]
fn outputs_that_fit_are_quoted_whole() -> Result<(), String> {
    let rows = outputs(8, 3_000)?;
    let (payload, quoted) = allocate(&rows, 4 * 1024 * 1024)?;
    for ((_, item), bytes) in rows.iter().zip(&quoted) {
        let CodexInputItem::ToolOutput { output, .. } = item else {
            return Err("fixture row is not a tool output".into());
        };
        assert_eq!(*bytes, output.as_str().len(), "{quoted:?}");
    }
    assert!(!payload.contains("summary-tool-prefix-v1"));
    Ok(())
}

#[test]
fn a_budget_below_every_floor_still_quotes_the_newest_output_first() -> Result<(), String> {
    let rows = outputs(10, 4_000)?;
    let session = session()?;
    let refs: Vec<_> = rows.iter().map(|(seq, item)| (*seq, item)).collect();
    let baseline = encode_summary_payload(None, &session, "continue", &refs, &[0; 10])?.len();
    let available = baseline + 2_000;
    let (payload, quoted) = allocate(&rows, available)?;
    assert!(payload.len() <= available);
    assert!(quoted[9] > 0 && quoted[0] == 0, "{quoted:?}");
    Ok(())
}

fn call(index: usize) -> Result<CodexInputItem, String> {
    Ok(CodexInputItem::ToolCall {
        call: ToolCallId::new(format!("call_pair_{index}")).ok_or("call")?,
        tool: grapt_provider::ToolName::new("read").ok_or("tool")?,
        arguments: CodexToolText::try_new("{}".to_owned()).map_err(|error| format!("{error:?}"))?,
    })
}

fn output(index: usize) -> Result<CodexInputItem, String> {
    Ok(CodexInputItem::ToolOutput {
        call: ToolCallId::new(format!("call_pair_{index}")).ok_or("call")?,
        output: CodexToolText::try_new("result".to_owned())
            .map_err(|error| format!("{error:?}"))?,
        is_error: false,
    })
}

fn image_output(index: usize) -> Result<CodexInputItem, String> {
    let image = grapt_provider::InputImage::try_new(
        grapt_provider::ImageMedia::Png,
        "YWJj".to_owned(),
        grapt_provider::ImageDetail::High,
        8,
        8,
    )?;
    Ok(CodexInputItem::ImageToolOutput {
        call: ToolCallId::new(format!("call_pair_{index}")).ok_or("call")?,
        output: CodexToolText::try_new("picture".to_owned())
            .map_err(|error| format!("{error:?}"))?,
        images: grapt_provider::InputImages::try_new(vec![image])?,
    })
}

/// The newest start at which `budget` rows still fit, with each row costing one.
fn start_with(items: &[CodexInputItem], budget: usize) -> Result<Option<usize>, String> {
    let rows: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(seq, item)| (seq as u64, item))
        .collect();
    quote_start(&rows, |skip| Ok(rows.len() - skip <= budget))
}

/// Calls and outputs interleave in commit order. A quote that starts after
/// call A but keeps output A would carry an output with no call to explain it,
/// so the oldest exchanges leave whole: dropping A means dropping outA too.
#[test]
fn dropping_the_oldest_rows_never_strands_an_output_without_its_call() -> Result<(), String> {
    // [call A, call B, output A, output B]
    let items = [call(0)?, call(1)?, output(0)?, output(1)?];
    // Room for three rows: the head quoted B and both outputs. Starting at B
    // would keep outA without call A, and starting at outA keeps it too.
    assert_eq!(
        start_with(&items, 3)?,
        None,
        "an output kept its call dropped"
    );
    // Room for all four: nothing is dropped.
    assert_eq!(start_with(&items, 4)?, Some(0));
    // Sequential exchanges drop whole: [call A, outA, call B, outB].
    let sequential = [call(0)?, output(0)?, call(1)?, output(1)?];
    assert_eq!(start_with(&sequential, 2)?, Some(2));
    // A single row of room cannot hold an exchange of two, and a start on an
    // output alone is never offered.
    assert_eq!(start_with(&sequential, 1)?, None);
    Ok(())
}

/// An image tool output is paired with its call exactly as a text output is.
#[test]
fn an_image_output_leaves_with_its_call() -> Result<(), String> {
    // [call A, call B, image output A, output B]
    let items = [call(0)?, call(1)?, image_output(0)?, output(1)?];
    assert_eq!(
        start_with(&items, 3)?,
        None,
        "an image output lost its call"
    );
    let sequential = [call(0)?, image_output(0)?, call(1)?, output(1)?];
    assert_eq!(start_with(&sequential, 2)?, Some(2));
    Ok(())
}
