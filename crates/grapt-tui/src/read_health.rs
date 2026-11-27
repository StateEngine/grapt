//! A read notice belongs to its exact request; unrelated successes cannot clear it.
use grapt_client::contract::{Method, Operation};
use grapt_client::{TransportError, WireRequest, WireResponse};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    operation: Operation,
    path: String,
    query: Vec<(String, String)>,
}
impl Key {
    fn read(request: &WireRequest) -> Option<Self> {
        (request.operation().method() == Method::Get).then(|| Self {
            operation: request.operation(),
            path: request.path().into(),
            query: request
                .query()
                .iter()
                .filter(|(key, _)| {
                    matches!(
                        key.as_str(),
                        "directory" | "location[directory]" | "session" | "project"
                    )
                })
                .cloned()
                .collect(),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    key: Key,
    message: String,
}

#[derive(Debug, Default)]
pub(crate) struct Health {
    pending: [Option<Key>; 3],
    notice: Option<Notice>,
    revision: u64,
    last_failed_read: bool,
}
impl Health {
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn message(&self) -> Option<String> {
        self.notice
            .as_ref()
            .map(|n| format!("{}: {}", n.key.path, n.message))
    }
    pub(crate) fn transient(&self, error: &grapt_client::ClientError) -> bool {
        self.last_failed_read
            && match error {
                grapt_client::ClientError::Transport(error) => read_failure(error),
                grapt_client::ClientError::Status { status, .. }
                | grapt_client::ClientError::Refused { status, .. } => {
                    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
                }
                _ => false,
            }
    }
    fn observe(&mut self, key: Option<Key>, result: Result<&WireResponse, &TransportError>) {
        let Some(key) = key else {
            self.write();
            return;
        };
        let next = match result {
            Ok(response) if (200..300).contains(&response.status) => {
                if self.notice.as_ref().is_none_or(|n| n.key != key) {
                    return;
                }
                None
            }
            Ok(response) if matches!(response.status, 408 | 429 | 500 | 502 | 503 | 504) => {
                Some(Notice {
                    key,
                    message: format!("service answered {}", response.status),
                })
            }
            Err(error) if read_failure(error) => Some(Notice {
                key,
                message: error.to_string(),
            }),
            _ => return,
        };
        self.last_failed_read |= next.is_some();
        if self.notice != next {
            self.notice = next;
            self.revision = self.revision.saturating_add(1);
        }
    }
    pub(super) fn call(
        &mut self,
        request: &WireRequest,
        result: &Result<WireResponse, TransportError>,
    ) {
        self.observe(Key::read(request), result.as_ref());
    }
    pub(super) fn begin(
        &mut self,
        lane: usize,
        request: &WireRequest,
        result: &Result<Option<WireResponse>, TransportError>,
    ) {
        self.pending[lane] = Key::read(request);
        self.poll(lane, result);
    }
    pub(super) fn poll(
        &mut self,
        lane: usize,
        result: &Result<Option<WireResponse>, TransportError>,
    ) {
        match result {
            Ok(None) => {}
            Ok(Some(response)) => {
                let key = self.pending[lane].take();
                self.observe(key, Ok(response));
            }
            Err(error) => {
                let key = self.pending[lane].take();
                self.observe(key, Err(error));
            }
        }
    }
    pub(super) fn write(&mut self) {
        self.last_failed_read = false;
    }
    pub(super) fn cancel(&mut self, lane: usize) {
        self.pending[lane] = None;
    }
}

// An unanswered GET can be retried. The same error from a write keeps its
// original uncertain-effect handling; do not broaden TransportError globally.
fn read_failure(error: &TransportError) -> bool {
    error.is_transient() || matches!(error, TransportError::AfterSend { .. })
}
