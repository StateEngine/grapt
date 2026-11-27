//! The fixed Windows bootstrap reads one version identity, never a path.

use std::path::Path;

#[cfg(target_os = "linux")]
use super::io;
#[cfg(windows)]
use super::windows_io as io;

use grapt_types::installation::{
    decode_pointer as decode, encode_pointer as encode, POINTER_BYTES,
};

// The transition owner validates the selected version directories and state.
// This adapter only reads/publishes the bounded Windows pointer format.
pub(super) fn read(prefix: &Path) -> Result<Option<String>, String> {
    io::directory(prefix)?;
    let path = prefix.join("current");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("installation current must be a regular pointer file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "installation current is unreadable: {:?}",
                error.kind()
            ))
        }
    }
    let bytes = io::read(&path, POINTER_BYTES as u64)?;
    decode(&bytes)
        .map(|identity| Some(identity.to_string()))
        .map_err(str::to_owned)
}

pub(super) fn publish(prefix: &Path, identity: &str) -> Result<(), String> {
    let bytes = encode(identity).map_err(str::to_owned)?;
    io::replace(&prefix.join("current"), &bytes)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, POINTER_BYTES};

    const BUILD: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn pointer_round_trip_keeps_the_exact_build() -> Result<(), &'static str> {
        let bytes = encode(BUILD)?;
        assert_eq!(bytes.len(), POINTER_BYTES);
        assert_eq!(decode(&bytes)?.as_str(), BUILD);
        Ok(())
    }

    #[test]
    fn incomplete_or_overlong_pointer_is_not_a_selection() -> Result<(), &'static str> {
        let bytes = encode(BUILD)?;
        for length in 0..POINTER_BYTES {
            assert!(decode(&bytes[..length]).is_err());
        }
        let mut overlong = bytes;
        overlong.push(b'\n');
        assert!(decode(&overlong).is_err());
        Ok(())
    }

    #[test]
    fn paths_and_uppercase_are_not_version_identities() {
        let uppercase = BUILD.to_ascii_uppercase();
        for value in ["versions/../outside", uppercase.as_str()] {
            assert!(encode(value).is_err());
            assert!(decode(format!("{value}\n").as_bytes()).is_err());
        }
    }

    #[test]
    fn nonhex_and_invalid_utf8_are_refused() -> Result<(), &'static str> {
        for bad in [b'g', b'/', b'\\', 0, 0xff] {
            let mut bytes = encode(BUILD)?;
            bytes[0] = bad;
            assert!(decode(&bytes).is_err());
        }
        Ok(())
    }

    #[test]
    fn crlf_and_trailing_space_are_not_silently_normalized() -> Result<(), &'static str> {
        let mut bytes = encode(BUILD)?;
        bytes[POINTER_BYTES - 1] = b' ';
        assert!(decode(&bytes).is_err());
        assert!(decode(format!("{BUILD}\r\n").as_bytes()).is_err());
        Ok(())
    }
}
