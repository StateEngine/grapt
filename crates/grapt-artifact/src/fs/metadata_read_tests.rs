use super::read_text_with_length;
use std::io::{self, Cursor, Read};

struct Counted {
    input: Cursor<Vec<u8>>,
    calls: usize,
    max_read: usize,
}

impl Counted {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            input: Cursor::new(bytes),
            calls: 0,
            max_read: usize::MAX,
        }
    }
}

impl Read for Counted {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        let take = buffer.len().min(self.max_read);
        self.input.read(&mut buffer[..take])
    }
}

#[test]
fn a_small_metadata_record_uses_one_payload_read_and_an_eof_probe() -> io::Result<()> {
    let bytes = vec![b'a'; 512];
    let mut reader = Counted::new(bytes.clone());
    let text = read_text_with_length(&mut reader, 512, 256 * 1024)?;
    assert_eq!(text.as_bytes(), bytes);
    assert!(
        reader.calls <= 2,
        "tiny repeated metadata reads: {}",
        reader.calls
    );
    Ok(())
}

#[test]
fn an_overbound_length_is_rejected_before_reading() {
    let mut reader = Counted::new(vec![b'a'; 65]);
    let result = read_text_with_length(&mut reader, 65, 64);
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
    assert_eq!(reader.calls, 0);
}

#[test]
fn growth_after_the_length_check_still_reads_only_limit_plus_one() {
    let mut reader = Counted::new(vec![b'a'; 128]);
    let result = read_text_with_length(&mut reader, 8, 64);
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidData));
    assert_eq!(reader.input.position(), 65);
}

#[test]
fn exact_bound_empty_and_invalid_utf8_keep_their_read_results() -> io::Result<()> {
    assert_eq!(read_text_with_length(&b"abcd"[..], 4, 4)?, "abcd");
    assert_eq!(read_text_with_length(&b""[..], 0, 4)?, "");
    assert!(matches!(read_text_with_length(&[0xff][..], 1, 4),
        Err(error) if error.kind() == io::ErrorKind::InvalidData));
    Ok(())
}

#[test]
fn short_reads_still_collect_the_complete_record() -> io::Result<()> {
    let bytes = vec![b'a'; 512];
    let mut reader = Counted::new(bytes.clone());
    reader.max_read = 7;
    let text = read_text_with_length(&mut reader, 512, 1024)?;
    assert_eq!(text.as_bytes(), bytes);
    assert!(reader.calls > 2);
    Ok(())
}

#[test]
fn shrinkage_after_observing_length_returns_the_actual_bytes() -> io::Result<()> {
    assert_eq!(read_text_with_length(&b"short"[..], 64, 64)?, "short");
    Ok(())
}

#[test]
fn a_read_failure_after_partial_content_preserves_the_io_error() {
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }
    let reader = (&b"partial"[..]).chain(Failing);
    assert!(matches!(read_text_with_length(reader, 64, 64),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
}
