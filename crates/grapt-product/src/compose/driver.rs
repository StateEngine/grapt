//! The service owns the prompt driver before and after first connection.
use crate::dogfood::{lifecycle::StopReason, prompt_driver::PromptDriver};

pub(crate) enum ServiceDriver {
    Ready(PromptDriver),
    Pending(tokio::sync::oneshot::Receiver<PromptDriver>),
}

impl ServiceDriver {
    pub(crate) async fn run_until_shutdown(
        &mut self,
        stop: impl std::future::Future<Output = StopReason>,
    ) -> Result<(), String> {
        tokio::pin!(stop);
        match self {
            Self::Ready(driver) => driver.run_until_shutdown(stop).await,
            Self::Pending(receiver) => tokio::select! {
                reason = &mut stop => { let _reason = reason; Ok(()) },
                driver = receiver => {
                    let mut driver = driver.map_err(|_| "provider setup owner stopped before activation")?;
                    driver.run_until_shutdown(stop).await
                },
            },
        }
    }
}
