use std::{net::SocketAddr, thread, time::Duration};

use rusplay_net::{NetError, Transport};

pub(crate) const SEND_RETRY_WAIT: Duration = Duration::from_micros(200);

#[derive(Debug)]
pub(crate) struct ForwardedDatagram {
    pub epoch: u32,
    pub bytes: Vec<u8>,
}

pub(crate) fn send_all(
    transport: &Transport,
    client: SocketAddr,
    datagrams: &[Vec<u8>],
) -> Result<(), NetError> {
    let mut sent = 0;

    while sent < datagrams.len() {
        let progress = transport.send_batch(&datagrams[sent..], client)?;
        if progress == 0 {
            thread::sleep(SEND_RETRY_WAIT);
        }
        sent += progress;
    }

    Ok(())
}
