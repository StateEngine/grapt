//! Match one bounded icacls /save /T result to the complete inspected tree.
use super::acl::{self, Descriptor};
use std::collections::BTreeMap;

fn key(name: &str) -> Result<String, String> {
    if name.is_empty() || name.len() > 4096 || name.contains([':', '\0', '\r', '\n']) {
        return Err("icacls saved an unsafe relative name".into());
    }
    let name = name.replace('/', "\\");
    if name
        .split('\\')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("icacls saved a noncanonical relative name".into());
    }
    Ok(name.to_lowercase())
}

pub(super) fn parse(bytes: &[u8], expected: &[String]) -> Result<Vec<Descriptor>, String> {
    if bytes.len() > 16 * 1024 * 1024
        || !bytes.len().is_multiple_of(2)
        || expected.is_empty()
        || expected.len() > super::bounds::TREE_ENTRIES
    {
        return Err("icacls saved inventory exceeds its byte or entry bound".into());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let units = units.strip_prefix(&[0xfeff]).unwrap_or(&units);
    let text = String::from_utf16(units).map_err(|_| "icacls saved invalid UTF-16")?;
    let lines: Vec<_> = text.lines().filter(|line| !line.is_empty()).collect();
    if lines.len() != expected.len() * 2 {
        return Err("icacls saved an incomplete or additional inventory".into());
    }
    let mut found = BTreeMap::new();
    for pair in lines.chunks_exact(2) {
        if !pair[1].starts_with("D:") {
            return Err("icacls saved an unexpected security descriptor".into());
        }
        let descriptor = acl::parse(pair[1])?;
        if found.insert(key(pair[0])?, descriptor).is_some() {
            return Err("icacls saved duplicate names".into());
        }
    }
    let descriptors = expected
        .iter()
        .map(|name| {
            found
                .remove(&key(name)?)
                .ok_or_else(|| "icacls did not save every inspected object".into())
        })
        .collect::<Result<Vec<_>, String>>()?;
    if !found.is_empty() {
        return Err("icacls saved objects outside the inspected tree".into());
    }
    Ok(descriptors)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encoded(value: &str) -> Vec<u8> {
        [0xfeff]
            .into_iter()
            .chain(value.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect()
    }
    #[test]
    fn batch_is_complete_order_independent_and_retains_mandatory_labels() -> Result<(), String> {
        let expected = vec!["build".into(), r"build\node.exe".into()];
        let bytes = encoded(
            "build\\node.exe\r\nD:(A;;FA;;;SY)S:(ML;;NW;;;ME)\r\nbuild\r\nD:(A;;FA;;;SY)\r\n",
        );
        let descriptors = parse(&bytes, &expected)?;
        assert_eq!(descriptors.len(), 2);
        let no_label = parse(&encoded("build\r\nD:(A;;FA;;;SY)\r\n"), &["build".into()])?;
        assert_ne!(descriptors[1], no_label[0]);
        Ok(())
    }
    #[test]
    fn incomplete_extra_duplicate_and_escaping_batches_refuse() {
        let expected = vec!["build".into(), r"build\node.exe".into()];
        for text in [
            "build\r\nD:(A;;FA;;;SY)\r\n",
            "build\r\nD:(A;;FA;;;SY)\r\nbuild\r\nD:(A;;FA;;;SY)\r\n",
            "build\r\nD:(A;;FA;;;SY)\r\nother\\node.exe\r\nD:(A;;FA;;;SY)\r\n",
            "build\r\nD:(A;;FA;;;SY)\r\n..\\node.exe\r\nD:(A;;FA;;;SY)\r\n",
        ] {
            assert!(parse(&encoded(text), &expected).is_err());
        }
        assert!(parse(&[0], &expected).is_err());
    }
}
