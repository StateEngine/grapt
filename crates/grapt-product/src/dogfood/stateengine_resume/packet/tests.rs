use super::*;
use serde_json::json;
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(in crate::dogfood::stateengine_resume) struct Fixture {
    directory: PathBuf,
    pub(in crate::dogfood::stateengine_resume) path: PathBuf,
}

impl Fixture {
    pub(in crate::dogfood::stateengine_resume) fn new(bytes: &[u8]) -> std::io::Result<Self> {
        let directory = std::env::temp_dir().join(format!(
            "grapt-launch-packet-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&directory)?;
        let path = directory.join("packet.md");
        std::fs::write(&path, bytes)?;
        Ok(Self { directory, path })
    }

    pub(in crate::dogfood::stateengine_resume) fn pointer(&self, bytes: &[u8]) -> Value {
        let sha256: String = ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        json!({"state":"written", "path":self.path, "bytes":bytes.len(), "sha256":sha256})
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

const BODY: &[u8] = b"# StateEngine\npacket_id: packet_exact\nWork on the admitted task.\n";

#[test]
fn verified_packet_requires_exact_bytes_and_distinct_delivery_failures(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new(BODY)?;
    let pointer = Pointer::parse(&fixture.pointer(BODY).to_string())?;
    let verified = read(&pointer)?;
    assert_eq!(verified.id(), Some("packet_exact"));
    assert_eq!(verified.text().as_bytes(), BODY);
    assert!(!format!("{verified:?} {pointer:?}").contains("packet_exact"));
    assert!(!format!("{verified:?} {pointer:?}").contains("packet.md"));
    // Same length defeats a length-only verifier; the digest must reject it.
    let mut corrupt = BODY.to_vec();
    corrupt[0] = b'!';
    std::fs::write(&fixture.path, &corrupt)?;
    assert_eq!(read(&pointer).err(), Some(E::PacketInvalid));
    std::fs::write(&fixture.path, b"truncated")?;
    assert_eq!(read(&pointer).err(), Some(E::PacketInvalid));
    std::fs::remove_file(&fixture.path)?;
    assert_eq!(read(&pointer).err(), Some(E::PacketMissing));
    std::fs::create_dir(&fixture.path)?;
    assert_eq!(read(&pointer).err(), Some(E::PacketUnreadable));
    Ok(())
}

#[test]
fn pointer_and_packet_structure_are_bounded_and_closed() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new(BODY)?;
    for (key, value) in [
        ("state", json!("REFUSED")),
        ("bytes", json!(0)),
        ("bytes", json!(MAX_PACKET + 1)),
        ("path", json!("relative.md")),
        ("path", json!("/bad\npath")),
        ("sha256", json!("F".repeat(64))),
        ("sha256", json!("a".repeat(63))),
        ("unexpected", json!(true)),
    ] {
        let mut raw = fixture.pointer(BODY);
        raw[key] = value;
        assert_eq!(
            Pointer::parse(&raw.to_string()).err(),
            Some(E::PacketInvalid)
        );
    }
    assert_eq!(
        Pointer::parse(&" ".repeat(MAX_POINTER + 1)).err(),
        Some(E::PacketInvalid)
    );
    for bytes in [
        b"no identifier".as_slice(),
        b"packet_id: \n",
        b"packet_id: a\npacket_id: b\n",
        b"packet_id: invalid id\n",
        b"packet_id: a\n\xff",
    ] {
        std::fs::write(&fixture.path, bytes)?;
        let pointer = Pointer::parse(&fixture.pointer(bytes).to_string())?;
        assert_eq!(read(&pointer).err(), Some(E::PacketInvalid));
    }
    Ok(())
}

#[test]
fn acknowledgement_must_record_the_exact_packet() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new(BODY)?;
    let packet = read(&Pointer::parse(&fixture.pointer(BODY).to_string())?)?;
    let good = json!({"packet_acknowledgement":{"state":"acknowledged","packet_id":"packet_exact","recorded":true}});
    let session: SessionId = "ses_packet_ack".parse()?;
    packet.require_acknowledgement(&good, "fixture", "agt_fixture", &session)?;
    for reply in [
        json!({}),
        json!({"packet_acknowledgement":{"state":"acknowledged","packet_id":"foreign","recorded":true}}),
        json!({"packet_acknowledgement":{"state":"acknowledged","packet_id":"packet_exact","recorded":false}}),
        json!({"packet_acknowledgement":{"state":"refused","packet_id":"packet_exact","recorded":true}}),
    ] {
        assert_eq!(
            packet.require_acknowledgement(&reply, "fixture", "agt_fixture", &session),
            Err(E::PacketAcknowledgement)
        );
    }
    Ok(())
}

#[test]
fn each_delivery_is_read_once_even_after_failure_until_rebound(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new(BODY)?;
    let pointer = Pointer::parse(&fixture.pointer(BODY).to_string())?;
    let session: SessionId = "ses_packet_once".parse()?;
    let mut cache = Cache::default();
    let reads = Cell::new(0);
    let load = |cache: &mut Cache, binding: &str| {
        cache.load_with(&session, binding, &pointer, |pointer| {
            reads.set(reads.get() + 1);
            read(pointer)
        })
    };
    let first = load(&mut cache, "binding")?;
    std::fs::remove_file(&fixture.path)?;
    assert!(Arc::ptr_eq(&first, &load(&mut cache, "binding")?));
    assert_eq!(reads.get(), 1);
    assert_eq!(
        load(&mut cache, "new binding").err(),
        Some(E::PacketMissing)
    );
    std::fs::write(&fixture.path, BODY)?;
    assert_eq!(
        load(&mut cache, "new binding").err(),
        Some(E::PacketMissing)
    );
    assert_eq!(reads.get(), 2, "failed delivery must not be reopened");
    cache.remove(&session);
    assert_eq!(cache.reserved, 0);
    load(&mut cache, "new binding")?;
    assert_eq!(reads.get(), 3, "explicit rebind admits a new delivery");
    Ok(())
}

#[test]
fn cache_admits_rows_and_retained_bytes_before_file_io() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new(BODY)?;
    let pointer = Pointer::parse(&fixture.pointer(BODY).to_string())?;
    let mut cache = Cache::default();
    for ordinal in 0..MAX_SESSIONS {
        let session = format!("ses_packet_{ordinal}").parse()?;
        assert_eq!(
            cache
                .load_with(&session, "binding", &pointer, |_| Err(E::PacketMissing))
                .err(),
            Some(E::PacketMissing)
        );
    }
    let session: SessionId = "ses_packet_overflow".parse()?;
    assert_eq!(
        cache
            .load_with(&session, "binding", &pointer, |_| panic!(
                "row overflow opened file"
            ))
            .err(),
        Some(E::Bounds)
    );
    cache = Cache::default();
    let oversized_binding = "b".repeat(MAX_RETAINED);
    assert_eq!(
        cache
            .load_with(&session, &oversized_binding, &pointer, |_| panic!(
                "byte overflow opened file"
            ))
            .err(),
        Some(E::Bounds)
    );
    assert_eq!(cache.reserved, 0);
    assert!(cache.entries.is_empty());
    Ok(())
}
