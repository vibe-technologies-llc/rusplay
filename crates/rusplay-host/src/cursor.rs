use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use rusplay_capture::{CursorBitmap, CursorUpdate, Received, Subscriber};
use rusplay_core::Resolution;
use rusplay_net::{FecScheme, Packetizer, Transport};
use rusplay_protocol::{CursorImage, CursorMessage, StreamKind};

use crate::{CaptureFeed, HostError, datagrams, shared::Shared};

const POLL: Duration = Duration::from_millis(20);

pub(crate) struct CursorLinks {
    pub shared: Arc<Shared>,
    pub feed: Arc<CaptureFeed>,
    pub subscriber: Subscriber<CursorUpdate>,
    pub transport: Arc<Transport>,
}

pub(crate) fn run(image_repeat: Option<Duration>, links: &CursorLinks) -> Result<(), HostError> {
    let mut state = links.feed.take_cursor_state();
    let result = serve(image_repeat, &mut state, links);
    links.feed.keep_cursor_state(state);
    links.shared.stop();

    result
}

fn serve(
    image_repeat: Option<Duration>,
    state: &mut Option<CursorUpdate>,
    links: &CursorLinks,
) -> Result<(), HostError> {
    let mut packetizer = Packetizer::new(
        StreamKind::Cursor,
        links
            .transport
            .max_datagram_len(&links.shared.client.address()),
        FecScheme::None,
    )?;
    let mut number = 0u32;
    let mut changed = state.is_some();
    let mut image_pending = true;
    let mut image_sent_at: Option<Instant> = None;
    let mut space_sent: Option<Resolution> = None;
    let mut epoch = links.shared.client.epoch();
    let counters = &links.shared.counters;

    while !links.shared.stopped() {
        let current_epoch = links.shared.client.epoch();
        if current_epoch != epoch {
            epoch = current_epoch;
            changed = true;
            image_pending = true;
            image_sent_at = None;
            space_sent = None;
        }

        match links.subscriber.recv_timeout(POLL) {
            Received::Value(update) => {
                image_pending |= update.bitmap.is_some();
                changed = true;
                *state = Some(match state.take() {
                    Some(previous) => update.absorb(previous),
                    None => update,
                });
            }
            Received::Empty => {}
            Received::Closed => thread::sleep(POLL),
        }

        let Some((current, space)) = state
            .as_ref()
            .zip(links.feed.capture_size())
            .filter(|_| links.shared.streaming())
        else {
            continue;
        };
        changed |= space_sent != Some(space);
        let now = Instant::now();
        let repeat_due = image_repeat
            .zip(image_sent_at)
            .is_some_and(|(interval, sent)| now.saturating_duration_since(sent) >= interval);
        if !changed && !repeat_due {
            continue;
        }

        let with_image = (image_pending || repeat_due) && current.bitmap.is_some();
        let message = message_for(current, space, with_image)?;
        let mut encoded = Vec::with_capacity(message.encoded_len());
        message.encode(&mut encoded)?;
        let datagrams = packetizer.packetize(number, false, &encoded)?;
        datagrams::send_all(&links.transport, links.shared.client.address(), &datagrams)?;

        number = number.wrapping_add(1);
        changed = false;
        space_sent = Some(space);
        counters.cursor_messages_sent.increment();
        if with_image {
            image_pending = false;
            image_sent_at = Some(now);
            counters.cursor_images_sent.increment();
        }
    }

    Ok(())
}

fn message_for(
    update: &CursorUpdate,
    space: Resolution,
    with_image: bool,
) -> Result<CursorMessage, HostError> {
    let image = update
        .bitmap
        .as_ref()
        .filter(|_| with_image)
        .map(image_for)
        .transpose()?;

    Ok(CursorMessage {
        x: update.x,
        y: update.y,
        space,
        visible: update.visible,
        image,
    })
}

fn image_for(bitmap: &CursorBitmap) -> Result<CursorImage, HostError> {
    let too_large = || HostError::CursorImageTooLarge {
        width: bitmap.width,
        height: bitmap.height,
    };
    let hotspot = |value: i32| u16::try_from(value.max(0)).map_err(|_| too_large());

    Ok(CursorImage {
        width: u16::try_from(bitmap.width).map_err(|_| too_large())?,
        height: u16::try_from(bitmap.height).map_err(|_| too_large())?,
        hotspot_x: hotspot(bitmap.hotspot_x)?,
        hotspot_y: hotspot(bitmap.hotspot_y)?,
        rgba: bitmap.rgba.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: Resolution = Resolution::new(3840, 2160);

    fn bitmap(width: u32, hotspot_x: i32) -> CursorBitmap {
        CursorBitmap {
            width,
            height: 1,
            hotspot_x,
            hotspot_y: 0,
            rgba: vec![7; width as usize * 4],
        }
    }

    fn update(bitmap: Option<CursorBitmap>) -> CursorUpdate {
        CursorUpdate {
            x: 3,
            y: -4,
            visible: true,
            bitmap,
        }
    }

    #[test]
    fn a_position_message_leaves_the_image_out() {
        let message = message_for(&update(Some(bitmap(2, 1))), SPACE, false).unwrap();

        assert_eq!((message.x, message.y, message.visible), (3, -4, true));
        assert_eq!(message.space, SPACE);
        assert_eq!(message.image, None);
    }

    #[test]
    fn an_image_message_carries_the_bitmap() {
        let message = message_for(&update(Some(bitmap(2, 1))), SPACE, true).unwrap();

        assert_eq!(
            message.image,
            Some(CursorImage {
                width: 2,
                height: 1,
                hotspot_x: 1,
                hotspot_y: 0,
                rgba: vec![7; 8],
            })
        );
    }

    #[test]
    fn a_negative_hotspot_clamps_to_the_corner() {
        let image = image_for(&bitmap(1, -5)).unwrap();

        assert_eq!(image.hotspot_x, 0);
    }

    #[test]
    fn a_bitmap_wider_than_the_wire_format_is_refused() {
        let oversized = bitmap(70_000, 0);

        assert!(matches!(
            image_for(&oversized),
            Err(HostError::CursorImageTooLarge { width: 70_000, .. })
        ));
    }

    #[test]
    fn an_update_without_a_bitmap_has_no_image_even_when_asked() {
        let message = message_for(&update(None), SPACE, true).unwrap();

        assert_eq!(message.image, None);
    }
}
