//! Hydrate quiescence through the registry's ordered offer port.
use crate::dogfood::execution::LiveActiveExecutions;
use grapt_api::DogfoodEventHub;
use std::sync::Arc;
pub(in crate::dogfood) fn install_hydration(
    active: Arc<LiveActiveExecutions>,
    status: Arc<super::SessionStatusEmitter>,
    (mail, continuity): (
        Arc<crate::dogfood::native_mail::custody::NativeMailOwners>,
        Arc<crate::dogfood::continuity_attention::ContinuityAttention>,
    ),
    hub: &DogfoodEventHub,
) {
    install_status_hydration(active, status, Some((mail, continuity)), hub);
}

pub(in crate::dogfood) fn install_volatile_hydration(
    active: Arc<LiveActiveExecutions>,
    status: Arc<super::SessionStatusEmitter>,
    hub: &DogfoodEventHub,
) {
    install_status_hydration(active, status, None, hub);
}

fn install_status_hydration(
    active: Arc<LiveActiveExecutions>,
    status: Arc<super::SessionStatusEmitter>,
    sidecars: Option<(
        Arc<crate::dogfood::native_mail::custody::NativeMailOwners>,
        Arc<crate::dogfood::continuity_attention::ContinuityAttention>,
    )>,
    hub: &DogfoodEventHub,
) {
    hub.install_status_hydrator(Box::new(move || {
        // Subscriber -> registry -> status is also the conditional live-offer
        // order, so a late retry and a new subscriber cannot invert locks.
        let mut frames = Vec::new();
        let listed = active.offer_status_hydration(&status, &mut frames);
        status.finish_hydration(&listed, &mut frames);
        // Degraded continuity goes ahead of mail, so the bounded offer
        // cannot crowd it out.
        if let Some((mail, continuity)) = &sidecars {
            frames.extend(continuity.hydrate());
            frames.extend(mail.hydrate());
        }
        frames
    }));
}
