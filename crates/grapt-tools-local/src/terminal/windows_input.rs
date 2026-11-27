//! ConPTY input for the native terminal.
//! Copyright 2025 OpenAI. Apache-2.0; Modified by Grapt:
//! private bounded writer integration and naming.

/// Enter is CR on ConPTY. Collapse CRLF even across separate stdin writes;
/// keep UTF-8 and control bytes intact, and translate backspace to DEL.
#[derive(Debug, Default)]
pub struct WindowsTtyInputNormalizer {
    previous_was_cr: bool,
}

impl WindowsTtyInputNormalizer {
    pub fn normalise(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut normalized = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            match byte {
                b'\x08' => normalized.push(b'\x7f'),
                b'\n' => {
                    if !self.previous_was_cr {
                        normalized.push(b'\r');
                    }
                }
                _ => normalized.push(byte),
            }
            self.previous_was_cr = byte == b'\r';
        }
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::WindowsTtyInputNormalizer;

    #[test]
    fn conpty_enter_submits_once_for_lf_crlf_and_split_crlf() {
        let mut input = WindowsTtyInputNormalizer::default();
        assert_eq!(input.normalise(b"first\n"), b"first\r");
        assert_eq!(input.normalise(b"second\r"), b"second\r");
        assert_eq!(input.normalise(b"\nthird\r\n"), b"third\r");
        assert_eq!(input.normalise(b"\n"), b"\r");
    }

    #[test]
    fn conpty_preserves_utf8_and_interrupt_and_translates_backspace() {
        let mut input = WindowsTtyInputNormalizer::default();
        let mut source = "café 漢字".as_bytes().to_vec();
        source.extend_from_slice(b"\x08\x03");
        let mut expected = "café 漢字".as_bytes().to_vec();
        expected.extend_from_slice(b"\x7f\x03");
        assert_eq!(input.normalise(&source), expected);
    }
}
