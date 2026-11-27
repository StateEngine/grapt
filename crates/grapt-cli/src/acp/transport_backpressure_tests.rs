use super::*;
use tokio::io::AsyncReadExt;
#[derive(Debug)]
struct FixedClock;
impl grapt_types::ClockAuthority for FixedClock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1)
    }
}
#[tokio::test]
async fn stopped_diagnostics_cannot_delay_the_next_acp_reply(
) -> Result<(), Box<dyn std::error::Error>> {
    let observer = super::super::timing::AcpTiming::bounded(Arc::new(FixedClock));
    let context = observer.context("ses_stopped_diagnostics".parse()?);
    let (outbound, inbound) = mpsc::channel(512);
    let (output, mut replies) = tokio::io::duplex(4096);
    let (diagnostics, _stopped_reader) = tokio::io::duplex(1);
    let writer = tokio::spawn(write_output(
        inbound,
        output,
        diagnostics,
        4096,
        Arc::clone(&observer),
    ));
    for id in [1, 2] {
        let (settled, completion) = oneshot::channel();
        outbound
            .send(Outbound::Message {
                value: json!({"id":id,"result":{}}),
                settled,
                timing: Some(context.clone()),
            })
            .await?;
        tokio::time::timeout(Duration::from_millis(250), completion).await???;
    }
    for _ in 0..256 {
        outbound
            .send(Outbound::Diagnostic("bounded witness".into()))
            .await?;
    }
    let (settled, completion) = oneshot::channel();
    outbound
        .send(Outbound::Message {
            value: json!({"id":3,"result":{}}),
            settled,
            timing: Some(context),
        })
        .await?;
    tokio::time::timeout(Duration::from_millis(250), completion).await???;
    assert!(
        observer.lost_at_least() > 0,
        "saturated diagnostics were not counted"
    );
    drop(outbound);
    tokio::time::timeout(Duration::from_millis(250), writer).await???;
    let mut bytes = Vec::new();
    replies.read_to_end(&mut bytes).await?;
    assert_eq!(
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        3
    );
    Ok(())
}
