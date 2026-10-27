use std::fmt;

use crate::ProtocolError;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ClientName {
    len: u8,
    bytes: [u8; ClientName::MAX_LEN],
}

impl ClientName {
    pub const MAX_LEN: usize = 64;
    pub const EMPTY: Self = Self {
        len: 0,
        bytes: [0; Self::MAX_LEN],
    };

    pub fn new(text: &str) -> Result<Self, ProtocolError> {
        Self::from_bytes(text.as_bytes())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > Self::MAX_LEN {
            return Err(ProtocolError::ClientNameTooLong {
                len: bytes.len(),
                max: Self::MAX_LEN,
            });
        }

        let text = std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidClientName)?;
        if text.chars().any(char::is_control) {
            return Err(ProtocolError::ClientNameControlCharacter);
        }

        Ok(Self::filled(text))
    }

    pub fn lossy(text: &str) -> Self {
        let mut kept = String::new();

        for character in text.chars().filter(|character| !character.is_control()) {
            if kept.len() + character.len_utf8() > Self::MAX_LEN {
                break;
            }
            kept.push(character);
        }

        Self::filled(&kept)
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn len_byte(&self) -> u8 {
        self.len
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    fn filled(text: &str) -> Self {
        let mut bytes = [0; Self::MAX_LEN];
        bytes[..text.len()].copy_from_slice(text.as_bytes());

        Self {
            len: u8::try_from(text.len()).unwrap_or(u8::MAX),
            bytes,
        }
    }
}

impl Default for ClientName {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl fmt::Debug for ClientName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ClientName")
            .field(&self.as_str())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_round_trips_through_its_bytes() {
        let name = ClientName::new("desk pc").unwrap();

        assert_eq!(name.as_str(), "desk pc");
        assert_eq!(ClientName::from_bytes(name.as_bytes()), Ok(name));
        assert!(!name.is_empty());
    }

    #[test]
    fn the_empty_name_is_valid_and_empty() {
        assert_eq!(ClientName::new(""), Ok(ClientName::EMPTY));
        assert!(ClientName::EMPTY.is_empty());
    }

    #[test]
    fn oversized_non_utf8_and_control_names_are_rejected() {
        assert_eq!(
            ClientName::new(&"n".repeat(65)),
            Err(ProtocolError::ClientNameTooLong { len: 65, max: 64 })
        );
        assert_eq!(
            ClientName::from_bytes(&[0xff, 0xfe]),
            Err(ProtocolError::InvalidClientName)
        );
        assert_eq!(
            ClientName::new("two\nlines"),
            Err(ProtocolError::ClientNameControlCharacter)
        );
        assert!(ClientName::new(&"n".repeat(64)).is_ok());
    }

    #[test]
    fn lossy_cuts_on_a_character_boundary_and_drops_control_characters() {
        let long = "é".repeat(40);

        let cut = ClientName::lossy(&long);
        let cleaned = ClientName::lossy("a\tb\nc");

        assert_eq!(cut.as_str(), "é".repeat(32));
        assert_eq!(cleaned.as_str(), "abc");
    }
}
