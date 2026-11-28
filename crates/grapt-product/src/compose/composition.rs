//! Application ports and service-owned execution custody.
use super::*;

pub(crate) struct ServerComposition {
    pub(crate) ports: Arc<grapt_api::u3a::ApplicationSlot>,
    pub(crate) integrations: Option<Arc<dyn grapt_api::u3a::IntegrationApplicationPort>>,
    pub(crate) installer: Option<Arc<dyn grapt_api::u3a::ClaudePluginApplicationPort>>,
    pub(crate) hub: Arc<DogfoodEventHub>,
    pub(crate) driver: Option<ServiceDriver>,
}

impl ServerComposition {
    pub(crate) fn ready(
        application: Arc<dyn ApplicationPort>,
        capture: Arc<dyn CaptureApplicationPort>,
        hub: Arc<DogfoodEventHub>,
        driver: Option<crate::dogfood::prompt_driver::PromptDriver>,
    ) -> Self {
        Self {
            ports: Arc::new(grapt_api::u3a::ApplicationSlot::ready(application, capture)),
            integrations: None,
            installer: None,
            hub,
            driver: driver.map(ServiceDriver::Ready),
        }
    }
}
