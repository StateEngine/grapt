// Probe 08 (R4H-H): the host's bounds cannot materialise on their own.
//
// A bound that can be omitted is a bound nobody chose. `ApiHostLimits` has
// eight fields, every one of them a checked type, and a `Default` would supply
// all eight from a number this crate invented — including the header-read and
// drain allowances, which decide whether a slow client is closed and whether a
// stop is graceful.
//
// This is the same law `MaxRouterModules` and `MaxPrincipalMappings` already
// carry in this crate, stated at the aggregate rather than only at the fields:
// a struct literal is not a bypass, because each field type has a private field
// and a fallible constructor, but a `Default` would be.
use grapt_api::ApiHostLimits;

fn main() {
    let _limits = ApiHostLimits::default();
}
