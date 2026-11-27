use crate::key::SubmitKey;
use crate::state::{Applied, Event};
use std::collections::{HashMap, VecDeque};

/// The most recent submit outcomes, so a retry is answered without a store
/// read. Older keys fall back to the durable transition record.
#[derive(Debug)]
pub(crate) struct Recent {
    capacity: usize,
    order: VecDeque<SubmitKey>,
    outcomes: HashMap<SubmitKey, (Event, Applied)>,
}

impl Recent {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity),
            outcomes: HashMap::with_capacity(capacity),
        }
    }

    pub(crate) fn get(&self, key: &SubmitKey) -> Option<&(Event, Applied)> {
        self.outcomes.get(key)
    }

    pub(crate) fn insert(&mut self, key: SubmitKey, event: Event, applied: Applied) {
        if self.capacity == 0 || self.outcomes.contains_key(&key) {
            return;
        }
        if self.order.len() == self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.outcomes.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.outcomes.insert(key, (event, applied));
    }
}
