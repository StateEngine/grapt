//! Native selection counterexamples run before a product opens its profile.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use grapt_service::installation::PrefixGuard;
use grapt_types::installation::encode_pointer;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "grapt-product-selection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }

    fn binary(&self, identity: &str) -> PathBuf {
        self.0
            .join("versions")
            .join(identity)
            .join("bin")
            .join("grapt-server.exe")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn windows_inactive_product_refuses_under_its_own_shared_guard() -> Result {
    let root = Scratch::new()?;
    std::fs::write(root.0.join("current"), encode_pointer(B)?)?;
    let guard = PrefixGuard::shared(&root.0)?;
    super::selected(&root.0, &root.binary(B))?;
    let refusal = super::selected(&root.0, &root.binary(A))
        .err()
        .ok_or("inactive product was admitted")?;
    println!("measured inactive-product restart action: {refusal}");
    assert!(refusal.contains("no longer active") && refusal.contains("bin\\grapt.exe"));
    assert!(PrefixGuard::exclusive(&root.0).is_err());
    drop(guard);
    let _exclusive = PrefixGuard::exclusive(&root.0)?;
    Ok(())
}

#[test]
fn windows_partial_corrupt_or_overlong_pointer_never_admits_product() -> Result {
    let root = Scratch::new()?;
    let correct = encode_pointer(A)?;
    let mut overlong = correct.clone();
    overlong.push(b'\n');
    let mut uppercase = correct.clone();
    uppercase[0] = b'A';
    let mut invalid = correct.clone();
    invalid[0] = 0xff;
    for bytes in [
        Vec::new(),
        correct[..40].to_vec(),
        overlong,
        uppercase,
        invalid,
    ] {
        std::fs::write(root.0.join("current"), bytes)?;
        let _guard = PrefixGuard::shared(&root.0)?;
        assert!(super::selected(&root.0, &root.binary(A)).is_err());
    }
    std::fs::write(root.0.join("current"), correct)?;
    let _guard = PrefixGuard::shared(&root.0)?;
    super::selected(&root.0, &root.binary(A))?;
    Ok(())
}

#[test]
fn windows_missing_or_directory_pointer_never_becomes_fresh_product_start() -> Result {
    let root = Scratch::new()?;
    let _guard = PrefixGuard::shared(&root.0)?;
    assert!(super::selected(&root.0, &root.binary(A)).is_err());
    std::fs::create_dir(root.0.join("current"))?;
    assert!(super::selected(&root.0, &root.binary(A))
        .is_err_and(|error| error.contains("regular pointer file")));
    Ok(())
}
