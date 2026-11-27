// Probe 11: a durable/lifecycle owner cannot obtain clock or ID authority
// without being handed both injected authorities. There is no Default and no
// zero-argument constructor, so an owner that reaches for ambient time or a
// random id has nowhere to get them from.
use grapt_types::DeterministicAuthorities;

fn main() {
    let _authorities = DeterministicAuthorities::new();
}
