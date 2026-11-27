// Probe 9: raw secret material has no Display, no Hash and no equality, so it
// cannot be formatted into a log, hashed into a public fingerprint, or compared
// as an offline equality oracle.
use grapt_types::Secret;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn main() {
    let secret = Secret::new(String::from("token"));
    println!("{}", secret);

    let mut hasher = DefaultHasher::new();
    secret.hash(&mut hasher);

    let other = Secret::new(String::from("token"));
    let _same = secret == other;
}
