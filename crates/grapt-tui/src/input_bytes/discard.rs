//! Refused controls retain only enough framing to consume their remaining bytes.
//! Matches the shared VT owner's bounded discard-through-final policy.

pub(super) enum Discard {
    Csi { introducer_only: bool },
    LegacyMouse(usize),
}

impl Discard {
    pub(super) fn unfinished(prefix: &[u8]) -> Option<Self> {
        match prefix {
            [0x1b, b'[', b'M', ..] if prefix.len() < 6 => Some(Self::LegacyMouse(6 - prefix.len())),
            [0x1b, b'[', ..] => Some(Self::Csi {
                introducer_only: prefix.len() == 2,
            }),
            [0x1b, b'O', ..] => Some(Self::Csi {
                introducer_only: false,
            }),
            _ => None,
        }
    }

    pub(super) fn consume(self, byte: u8) -> Option<Self> {
        match self {
            // ESC [ may expire before the legacy mouse marker arrives.
            Self::Csi {
                introducer_only: true,
            } if byte == b'M' => Some(Self::LegacyMouse(3)),
            Self::Csi { .. } if !(0x40..=0x7e).contains(&byte) => Some(Self::Csi {
                introducer_only: false,
            }),
            Self::LegacyMouse(left) if left > 1 => Some(Self::LegacyMouse(left - 1)),
            _ => None,
        }
    }
}
