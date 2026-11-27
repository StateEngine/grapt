//! A snapshot's rows install through the legacy page decoder in the exact
//! tag-first shape the server's typed encoder serves.
use super::decode;

#[test]
fn snapshot_rows_keep_their_union_tags_first_through_the_page_decoder(
) -> Result<(), Box<dyn std::error::Error>> {
    let body = br#"{"revision":{"aggregate":"ses_000000000001","seq":7},"rows":[
        {"type":"user","id":"msg_a964dd3bcd09f81e59a7d8ef0f386e78x0","time":{"created":1.0},"text":"hello"},
        {"type":"assistant","id":"msg_5e561a4dcdf3ddb7ac8573b8223e9694x19","time":{"created":2.0,"completed":3.0},
         "agent":"default","model":{"id":"dogfood-1","providerID":"openai"},
         "content":[{"type":"text","text":"hello","state":{"durableCapture":"complete"}}],"finish":"stop"}
    ],"cursor":{},"permissions":[],"forms":[],"execution":{},"provisional":[]}"#;
    let snapshot = decode(body).map_err(|error| error.to_string())?;
    assert_eq!(snapshot.seq, 7);
    assert_eq!(snapshot.page.messages.len(), 2);
    Ok(())
}
