// Probe 06 (R4H-A): sealing the router is final.
//
// The preflight proposed a runtime `RouterSealed` refusal for this. It is not
// needed and would have been weaker: `seal` takes `self` by VALUE, so a
// registration after sealing is a move-checker error and the sealed route set
// is immutable by construction rather than by a flag someone has to check.
//
// That is also why `ApiRefusal` carries no `RouterSealed` variant. A refusal
// that can never be constructed is a vocabulary entry that lies about what can
// happen, and this probe is what lets the variant be deleted rather than kept
// "just in case".
use grapt_api::{ApiRouterBuilder, MaxRouterModules, RouteMethod, RoutePath};

async fn ok() {}

fn main() {
    let Ok(bound) = MaxRouterModules::try_new(4) else {
        return;
    };
    let Ok(path) = RoutePath::try_new("/seats") else {
        return;
    };
    let mut builder = ApiRouterBuilder::new(bound);
    let _sealed = builder.seal();
    let _late = builder.register(RouteMethod::Get, path, ok);
}
