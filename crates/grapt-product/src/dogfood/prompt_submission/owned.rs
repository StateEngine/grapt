//! Shared service-owned execution and terminal publication for admitted work.
use super::*;

impl DogfoodApplication {
    pub(in crate::dogfood) async fn execute_owned_prompt(
        &self,
        request: ExecutionRequest,
        background: Option<background_child::BackgroundChild>,
    ) {
        let session = request.session.clone();
        let start_report = request.started.clone();
        let mut background = background;
        let mut result = self.executor.run(request).await;
        if let Some(background) = &mut background {
            if let Err(error) = background.finish(&result) {
                result = Err(backend_result::ExecutionFailure::Unpublished(error));
            }
        }
        if let Err(backend_result::ExecutionFailure::Unpublished(error)) = &result {
            self.hub.invalidate_because(&format!(
                "session {session}: prompt execution failed: {error}"
            ));
        }
        start_report.settle(result.map_err(backend_result::ExecutionFailure::into_reason));
    }
}
