use super::*;

#[derive(Debug)]
pub(in crate::dogfood) struct RuntimeClock(pub(in crate::dogfood) Arc<SystemClock>);

impl Clock for RuntimeClock {
    fn now_ms(&self) -> i64 {
        self.0.now().as_millis()
    }
}

#[derive(Debug)]
pub(in crate::dogfood) struct RuntimeEventIds(pub(in crate::dogfood) Arc<OsRandomIds>);

impl EventIds for RuntimeEventIds {
    fn mint(&self) -> EventId {
        match format!("evt_{}", self.0.mint().as_str()).parse() {
            Ok(event) => event,
            Err(_) => unreachable!("the event prefix plus a minted token is a valid EventId"),
        }
    }
}
