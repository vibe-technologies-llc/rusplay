use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU32, Ordering},
};

use parking_lot::RwLock;

#[derive(Debug)]
pub(crate) struct ClientLink {
    address: RwLock<SocketAddr>,
    epoch: AtomicU32,
}

impl ClientLink {
    pub(crate) fn new(address: SocketAddr) -> Self {
        Self {
            address: RwLock::new(address),
            epoch: AtomicU32::new(0),
        }
    }

    pub(crate) fn address(&self) -> SocketAddr {
        *self.address.read()
    }

    pub(crate) fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn switch(&self, address: SocketAddr) {
        *self.address.write() = address;
        self.epoch.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_moves_the_address_and_starts_a_new_epoch() {
        let first = SocketAddr::from(([127, 0, 0, 1], 4000));
        let second = SocketAddr::from(([127, 0, 0, 1], 5000));
        let link = ClientLink::new(first);

        let before = (link.address(), link.epoch());
        link.switch(second);

        assert_eq!(before, (first, 0));
        assert_eq!((link.address(), link.epoch()), (second, 1));
    }
}
