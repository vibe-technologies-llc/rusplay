use std::{sync::Arc, time::Instant};

use parking_lot::{Mutex, MutexGuard};
use rusplay_net::{ControlChannel, NetError, Transport};
use rusplay_protocol::{ControlMessage, StreamDescription};

use crate::{HostError, link::ClientLink};

#[derive(Debug, Default)]
struct Announcement {
    latest: Option<StreamDescription>,
    unsent: bool,
}

#[derive(Debug)]
pub(crate) struct ControlOutbox {
    channel: Mutex<ControlChannel>,
    announcement: Mutex<Announcement>,
    transport: Arc<Transport>,
    client: Arc<ClientLink>,
}

impl ControlOutbox {
    pub(crate) fn new(
        channel: ControlChannel,
        transport: Arc<Transport>,
        client: Arc<ClientLink>,
    ) -> Self {
        Self {
            channel: Mutex::new(channel),
            announcement: Mutex::new(Announcement::default()),
            transport,
            client,
        }
    }

    pub(crate) fn channel(&self) -> MutexGuard<'_, ControlChannel> {
        self.channel.lock()
    }

    pub(crate) fn flush(&self) -> Result<(), HostError> {
        self.channel
            .lock()
            .flush(&self.transport, self.client.address())?;

        Ok(())
    }

    pub(crate) fn reset(&self) {
        self.channel.lock().reset();
        self.announcement.lock().unsent = false;
    }

    pub(crate) fn resend_announcement(&self, now: Instant) -> Result<(), HostError> {
        let mut announcement = self.announcement.lock();
        announcement.unsent = announcement.latest.is_some();
        drop(announcement);

        self.send_announcement(now)
    }

    pub(crate) fn announce(&self, description: StreamDescription) -> Result<(), HostError> {
        let mut announcement = self.announcement.lock();
        announcement.latest = Some(description);
        announcement.unsent = true;
        drop(announcement);

        self.send_announcement(Instant::now())
    }

    pub(crate) fn reannounce(&self, lost: &StreamDescription) {
        let mut announcement = self.announcement.lock();
        if announcement.latest.as_ref() == Some(lost) {
            announcement.unsent = true;
        }
    }

    pub(crate) fn send_announcement(&self, now: Instant) -> Result<(), HostError> {
        let mut announcement = self.announcement.lock();
        let Some(description) = announcement.latest.filter(|_| announcement.unsent) else {
            return Ok(());
        };

        let mut channel = self.channel.lock();
        match channel.send(ControlMessage::StreamFormat(description), now) {
            Ok(_) => announcement.unsent = false,
            Err(NetError::ControlWindowFull { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        drop(announcement);

        channel.flush(&self.transport, self.client.address())?;

        Ok(())
    }
}
