//! An external dependent may inspect a store but cannot obtain the elected
//! controller's mutation permit and therefore cannot invoke raw-store CAS.

use grapt_service::{
    ElectedControllerAuthority, FileRegistrationStore, LockAttemptPolicy, RegistrationStore,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let policy = LockAttemptPolicy::new(1).ok_or("policy")?;
    let store = FileRegistrationStore::new("registration", policy);
    let authority = ElectedControllerAuthority::isolated();
    let permit = authority.mutation_permit();
    let _ = store.compare_and_swap(permit, None, Some(b"forged\n"))?;
    Ok(())
}
