use super::*;
use grapt_types::timing::TimingStage;
#[derive(Debug)]
struct TickClock(AtomicU64);
impl ClockAuthority for TickClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.fetch_add(5, Ordering::Relaxed) as i64)
    }
}
#[test]
fn acp_boundaries_use_the_injected_clock_and_survive_a_full_queue(
) -> Result<(), Box<dyn std::error::Error>> {
    let observer = AcpTiming::bounded(Arc::new(TickClock(AtomicU64::new(100))));
    let context = observer.context("ses_acp_tail".parse()?);
    for (index, stage) in [
        TimingStage::AcpTerminal,
        TimingStage::AcpFinalGet,
        TimingStage::AcpEnqueue,
        TimingStage::AcpWrite,
        TimingStage::AcpFlush,
    ]
    .into_iter()
    .enumerate()
    {
        drop(context.span(stage));
        let (samples, lost) = observer.drain();
        assert_eq!(lost, 0);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].stage, stage);
        assert_eq!(samples[0].scope, context.scope);
        assert_eq!(samples[0].start.as_millis(), 100 + index as i64 * 10);
        assert_eq!(samples[0].end.as_millis(), 105 + index as i64 * 10);
    }
    for _ in 0..CAPACITY + 2 {
        drop(context.span(TimingStage::AcpEnqueue));
    }
    let (samples, lost) = observer.drain();
    assert_eq!(samples.len(), CAPACITY);
    assert_eq!(lost, 2);
    drop(context.span(TimingStage::AcpFlush));
    assert_eq!(observer.drain().0.len(), 1);
    Ok(())
}
