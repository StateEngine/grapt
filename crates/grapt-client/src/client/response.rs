//! Bound response bodies and retain typed native refusal messages.
use super::*;

impl<C: Connection> Client<C> {
    pub(super) fn accept_response(
        &self,
        operation: Operation,
        response: WireResponse,
    ) -> Result<Vec<u8>, ClientError> {
        if !response.is_success() {
            // A snapshot route the service never registered answers a bare
            // 404; a session it cannot find answers a typed one, kept as is.
            if operation == Operation::SessionSnapshot && response.status == 404 {
                if let Some(refusal) =
                    super::snapshot::missing(&response.body, self.bounds.max_response_bytes)
                {
                    return Err(refusal);
                }
            }
            // A 400 that speaks the frozen error shape is READ before it is
            // refused: the product's message says what the config fault is,
            // and a client that dropped it would leave the user a status
            // number with no cause. The read stays inside the client's byte
            // bound, though: a bound one error shape can walk around is not
            // a bound, and an oversized error body must not become retained
            // pane state. Any other body, oversized or not, keeps the plain
            // status, unchanged.
            if response.status == 400 && response.body.len() <= self.bounds.max_response_bytes {
                if matches!(
                    operation,
                    Operation::SessionFormReply | Operation::GlobalFormReply
                ) {
                    if let Ok((form, message)) =
                        crate::wire::decode_form_invalid_answer(&response.body)
                    {
                        return Err(ClientError::FormInvalidAnswer {
                            operation: operation.operation_id(),
                            form,
                            message,
                        });
                    }
                }
                if let Ok(message) = crate::wire::decode_invalid_request_message(&response.body) {
                    return Err(ClientError::Refused {
                        operation: operation.operation_id(),
                        status: response.status,
                        message,
                    });
                }
            }
            if response.status == 409
                && matches!(
                    operation,
                    Operation::SessionUndoPreview
                        | Operation::SessionUndo
                        | Operation::SessionReconcile
                        // A prompt refused by a lost tool run names /reconcile.
                        | Operation::SessionPrompt
                )
                && response.body.len() <= self.bounds.max_response_bytes
            {
                if let Ok(message) = crate::wire::decode_undo_conflict_message(&response.body) {
                    return Err(ClientError::Refused {
                        operation: operation.operation_id(),
                        status: response.status,
                        message,
                    });
                }
            }
            return Err(ClientError::Status {
                operation: operation.operation_id(),
                status: response.status,
            });
        }
        // The response byte bound, enforced at INGRESS rather than declared,
        // and now on EVERY operation rather than on the subset that happened to
        // use `raw`.
        if response.body.len() > self.bounds.max_response_bytes {
            return Err(ClientError::Transport(TransportError::ResponseTooLarge {
                bound: self.bounds.max_response_bytes,
            }));
        }
        if operation == Operation::SessionDelete
            && (response.status != 204 || !response.body.is_empty())
        {
            return Err(ClientError::Body {
                operation: operation.operation_id(),
                reason: "Expected an empty HTTP 204 deletion acknowledgement".into(),
            });
        }
        Ok(response.body)
    }
}
