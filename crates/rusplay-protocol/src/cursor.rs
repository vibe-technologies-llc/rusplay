use crate::ProtocolError;

const POSITION_SIZE: usize = 9;
const IMAGE_HEADER_SIZE: usize = 8;
const BYTES_PER_PIXEL: usize = 4;
const VISIBLE: u8 = 1;
const HAS_IMAGE: u8 = 1 << 1;
const KNOWN_FLAGS: u8 = VISIBLE | HAS_IMAGE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    pub width: u16,
    pub height: u16,
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    pub rgba: Vec<u8>,
}

impl CursorImage {
    pub fn pixel_bytes(width: u16, height: u16) -> usize {
        usize::from(width) * usize::from(height) * BYTES_PER_PIXEL
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMessage {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub image: Option<CursorImage>,
}

impl CursorMessage {
    pub fn encoded_len(&self) -> usize {
        POSITION_SIZE
            + self
                .image
                .as_ref()
                .map_or(0, |image| IMAGE_HEADER_SIZE + image.rgba.len())
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        if let Some(image) = &self.image {
            let expected = CursorImage::pixel_bytes(image.width, image.height);
            if image.rgba.len() != expected {
                return Err(ProtocolError::CursorImageSizeMismatch {
                    expected,
                    found: image.rgba.len(),
                });
            }
        }

        let mut flags = 0;
        if self.visible {
            flags |= VISIBLE;
        }
        if self.image.is_some() {
            flags |= HAS_IMAGE;
        }

        out.reserve(self.encoded_len());
        out.extend_from_slice(&self.x.to_be_bytes());
        out.extend_from_slice(&self.y.to_be_bytes());
        out.push(flags);
        if let Some(image) = &self.image {
            for field in [image.width, image.height, image.hotspot_x, image.hotspot_y] {
                out.extend_from_slice(&field.to_be_bytes());
            }
            out.extend_from_slice(&image.rgba);
        }
        Ok(())
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let Some((position, rest)) = buf.split_first_chunk::<POSITION_SIZE>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: POSITION_SIZE,
                available: buf.len(),
            });
        };

        let x = i32::from_be_bytes([position[0], position[1], position[2], position[3]]);
        let y = i32::from_be_bytes([position[4], position[5], position[6], position[7]]);
        let flags = position[8];
        if flags & !KNOWN_FLAGS != 0 {
            return Err(ProtocolError::UnknownCursorFlags(flags));
        }

        let image = if flags & HAS_IMAGE == 0 {
            if !rest.is_empty() {
                return Err(ProtocolError::TrailingBytes(rest.len()));
            }
            None
        } else {
            Some(decode_image(rest, buf.len())?)
        };

        Ok(Self {
            x,
            y,
            visible: flags & VISIBLE != 0,
            image,
        })
    }
}

fn decode_image(rest: &[u8], total: usize) -> Result<CursorImage, ProtocolError> {
    let Some((header, rgba)) = rest.split_first_chunk::<IMAGE_HEADER_SIZE>() else {
        return Err(ProtocolError::BufferTooShort {
            needed: POSITION_SIZE + IMAGE_HEADER_SIZE,
            available: total,
        });
    };

    let field = |at: usize| u16::from_be_bytes([header[at], header[at + 1]]);
    let (width, height) = (field(0), field(2));
    let expected = CursorImage::pixel_bytes(width, height);
    if rgba.len() != expected {
        return Err(ProtocolError::CursorImageSizeMismatch {
            expected,
            found: rgba.len(),
        });
    }

    Ok(CursorImage {
        width,
        height,
        hotspot_x: field(4),
        hotspot_y: field(6),
        rgba: rgba.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_image() -> CursorMessage {
        CursorMessage {
            x: -12,
            y: 1080,
            visible: true,
            image: Some(CursorImage {
                width: 2,
                height: 1,
                hotspot_x: 1,
                hotspot_y: 0,
                rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
            }),
        }
    }

    fn round_trip(message: &CursorMessage) -> CursorMessage {
        let mut buf = Vec::new();
        message.encode(&mut buf).unwrap();
        assert_eq!(buf.len(), message.encoded_len());
        CursorMessage::decode(&buf).unwrap()
    }

    #[test]
    fn message_with_image_round_trips() {
        let message = with_image();

        assert_eq!(round_trip(&message), message);
    }

    #[test]
    fn position_only_message_round_trips() {
        let message = CursorMessage {
            x: 5,
            y: 6,
            visible: false,
            image: None,
        };

        assert_eq!(round_trip(&message), message);
        assert_eq!(message.encoded_len(), POSITION_SIZE);
    }

    #[test]
    fn image_with_wrong_pixel_count_is_not_encoded() {
        let mut message = with_image();
        if let Some(image) = &mut message.image {
            image.rgba.pop();
        }

        assert_eq!(
            message.encode(&mut Vec::new()),
            Err(ProtocolError::CursorImageSizeMismatch {
                expected: 8,
                found: 7
            })
        );
    }

    #[test]
    fn truncated_image_is_rejected() {
        let mut buf = Vec::new();
        with_image().encode(&mut buf).unwrap();

        buf.pop();

        assert_eq!(
            CursorMessage::decode(&buf),
            Err(ProtocolError::CursorImageSizeMismatch {
                expected: 8,
                found: 7
            })
        );
    }

    #[test]
    fn short_and_malformed_buffers_are_rejected() {
        let mut position_only = Vec::new();
        CursorMessage {
            x: 0,
            y: 0,
            visible: true,
            image: None,
        }
        .encode(&mut position_only)
        .unwrap();

        let mut unknown_flags = position_only.clone();
        unknown_flags[8] = 0x80;
        let mut trailing = position_only.clone();
        trailing.push(0);

        assert!(matches!(
            CursorMessage::decode(&position_only[..4]),
            Err(ProtocolError::BufferTooShort { .. })
        ));
        assert_eq!(
            CursorMessage::decode(&unknown_flags),
            Err(ProtocolError::UnknownCursorFlags(0x80))
        );
        assert_eq!(
            CursorMessage::decode(&trailing),
            Err(ProtocolError::TrailingBytes(1))
        );
    }
}
