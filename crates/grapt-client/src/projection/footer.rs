use super::*;

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Clock {
    boundary: bool,
    first: Option<Option<u64>>,
}

impl Clock {
    pub(super) fn boundary(&mut self) {
        self.boundary = true;
        self.first = None;
    }

    fn observe(&mut self, created: Option<u64>) -> Option<u64> {
        if !self.boundary {
            return None;
        }
        *self.first.get_or_insert(created)
    }
}

impl SessionView {
    pub(super) fn push_footer(
        &mut self,
        footer: Option<&crate::wire::AssistantFooter>,
        finish: Option<&crate::wire::DurableFinish>,
        error: Option<&crate::wire::DurableError>,
        bounds: Bounds,
    ) {
        let started = self
            .footer_clock
            .observe(footer.and_then(|footer| footer.created_ms));
        let Some(outcome) = Self::durable_outcome(finish, error) else {
            return;
        };
        self.footer_clock = Clock::default();
        let Some(footer) = footer else {
            return;
        };
        let mut footer = Box::new(footer.clone());
        footer.execution_started_ms = started;
        footer.outcome = Some(outcome);
        self.push_slice(SliceKind::AssistantFooter, "", bounds);
        if let Some(slice) = self.slices.last_mut() {
            slice.footer = Some(footer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Clock;

    #[test]
    fn missing_first_start_never_becomes_the_last_step_start() {
        let mut clock = Clock::default();
        assert_eq!(clock.observe(Some(100)), None);
        clock.boundary();
        assert_eq!(clock.observe(None), None);
        assert_eq!(clock.observe(Some(200)), None);
        clock.boundary();
        assert_eq!(clock.observe(Some(300)), Some(300));
        assert_eq!(clock.observe(Some(400)), Some(300));
    }
}
