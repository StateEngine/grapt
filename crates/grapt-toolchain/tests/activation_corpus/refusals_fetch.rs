use crate::infra::*;

#[test]
fn a_schemed_source_refuses_source_unsupported() {
    // The typed value names a source; the shipped adapter admits only
    // scheme-less relative paths. A scheme prefix is the explicit
    // release-input boundary, refused, never a silent capability.
    let digest = Sha256::of(b"archive").hex();
    let scene = Scene::fresh("r11", true);
    scene.write(
        PIN_REL,
        pin_text(
            "15.1.0",
            &[(TARGET, &digest, ENTRY, "https://example.com/rg.tar")],
        ),
    );
    scene.commit(&[PIN_REL]);
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::SourceUnsupported,
        "schemed source",
    );
    assert_attempt_clean(&scene);
    scene.cleanup();
}

#[test]
fn an_undeliverable_archive_refuses_source_fetch_failed() {
    // Missing file.
    let digest = Sha256::of(b"archive").hex();
    let scene = Scene::fresh("r13a", true);
    scene.write(
        PIN_REL,
        pin_text(
            "15.1.0",
            &[(TARGET, &digest, ENTRY, "archives/missing.tar")],
        ),
    );
    scene.commit(&[PIN_REL]);
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::SourceFetchFailed,
        "missing archive",
    );
    scene.cleanup();

    // Over the 8 MiB bound.
    let scene = Scene::fresh("r13b", true);
    let mut big = vec![0u8; 8 * 1024 * 1024 + 1];
    let last = big.len() - 1;
    big[last] = 1;
    scene.write("archives/big.tar", big);
    scene.write(
        PIN_REL,
        pin_text("15.1.0", &[(TARGET, &digest, ENTRY, "archives/big.tar")]),
    );
    scene.commit(&[PIN_REL]);
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::SourceFetchFailed,
        "overbound archive",
    );
    scene.cleanup();
}

#[test]
fn a_digest_mismatch_refuses_before_anything_is_staged() {
    let archive = archive_stream(&[file_entry(ENTRY, b"fake-ripgrep-binary-v1")]);
    let forged = Sha256::of(b"a different archive").hex();
    let scene = scene_with_input(
        "r14",
        "15.1.0",
        &[(TARGET, &forged, ENTRY, SOURCE_REL)],
        archive,
    );
    let source = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&source));
    expect_reason(
        &refusal,
        RefusalReason::ArchiveDigestMismatch,
        "forged digest",
    );
    assert_eq!(
        refusal.context.expected_sha256.as_deref(),
        Some(forged.as_str()),
        "corpus: the refusal names the expected digest"
    );
    assert!(
        refusal.context.actual_sha256.is_some(),
        "corpus: the refusal names the actual digest"
    );
    assert!(
        !LAYOUT.canonical(&scene.root, "ripgrep", ENTRY).exists(),
        "corpus: nothing may stand at the canonical path after a mismatch"
    );
    assert_attempt_clean(&scene);
    scene.cleanup();
}

#[test]
fn a_missing_entry_refuses_archive_missing_binary() {
    let archive = archive_stream(&[file_entry("other.bin", b"not the pinned entry")]);
    let digest = Sha256::of(&archive).hex();
    let scene = scene_with_input(
        "r15",
        "15.1.0",
        &[(TARGET, &digest, ENTRY, SOURCE_REL)],
        archive,
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::ArchiveMissingBinary,
        "entry absent from a verified archive",
    );
    assert_attempt_clean(&scene);
    scene.cleanup();
}

#[test]
fn unsafe_archive_entries_refuse_archive_unsafe_entry() {
    // A symlink entry.
    let mut stream = raw_entry(ENTRY, 0, b'1');
    stream.extend([0u8; BLOCK * 2]);
    let digest = Sha256::of(&stream).hex();
    let scene = scene_with_input(
        "r16a",
        "15.1.0",
        &[(TARGET, &digest, ENTRY, SOURCE_REL)],
        stream,
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::ArchiveUnsafeEntry,
        "symlink entry",
    );
    scene.cleanup();

    // A dot-segment path.
    let archive = archive_stream(&[file_entry("../evil", b"x")]);
    let digest = Sha256::of(&archive).hex();
    let scene = scene_with_input(
        "r16b",
        "15.1.0",
        &[(TARGET, &digest, ENTRY, SOURCE_REL)],
        archive,
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::ArchiveUnsafeEntry,
        "dot segment",
    );
    scene.cleanup();

    // The 17th entry.
    let mut parts = Vec::new();
    for index in 0..17usize {
        parts.push(file_entry(&format!("e{index}"), b"x"));
    }
    let archive = archive_stream(&parts);
    let digest = Sha256::of(&archive).hex();
    let scene = scene_with_input(
        "r16c",
        "15.1.0",
        &[(TARGET, &digest, ENTRY, SOURCE_REL)],
        archive,
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::ArchiveUnsafeEntry,
        "17th entry",
    );
    scene.cleanup();

    // A duplicate entry path.
    let archive = archive_stream(&[file_entry(ENTRY, b"one"), file_entry(ENTRY, b"two")]);
    let digest = Sha256::of(&archive).hex();
    let scene = scene_with_input(
        "r16d",
        "15.1.0",
        &[(TARGET, &digest, ENTRY, SOURCE_REL)],
        archive,
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::ArchiveUnsafeEntry,
        "duplicate entry",
    );
    scene.cleanup();
}

#[test]
fn a_bad_checksum_stream_refuses_archive_malformed() {
    // A corrupted header byte: the checksum no longer verifies. A header
    // the decoder cannot verify is malformed structure, not a short stream.
    let mut entry = file_entry(ENTRY, b"bin");
    entry[100] ^= 0xff;
    let stream = archive_stream(&[entry]);
    let digest = Sha256::of(&stream).hex();
    let scene = scene_with_input(
        "r17a",
        "15.1.0",
        &[(TARGET, &digest, ENTRY, SOURCE_REL)],
        stream,
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::ArchiveMalformed,
        "bad checksum",
    );
    scene.cleanup();
}

#[test]
fn a_structurally_inconsistent_stream_refuses_archive_truncated() {
    // A stream cut off its terminator.
    let full = archive_stream(&[file_entry(ENTRY, b"binary-bytes")]);
    let cut = full[..full.len() - BLOCK * 3].to_vec();
    let digest = Sha256::of(&cut).hex();
    let scene = scene_with_input(
        "r17b",
        "15.1.0",
        &[(TARGET, &digest, ENTRY, SOURCE_REL)],
        cut,
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::ArchiveTruncated,
        "cut stream",
    );
    scene.cleanup();
}
