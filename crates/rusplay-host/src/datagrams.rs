use std::{net::SocketAddr, thread};

use rusplay_net::{NetError, Transport};

pub(crate) fn send_all(
    transport: &Transport,
    client: SocketAddr,
    datagrams: &[Vec<u8>],
) -> Result<(), NetError> {
    let mut sent = 0;

    while sent < datagrams.len() {
        let progress = transport.send_batch(&datagrams[sent..], client)?;
        if progress == 0 {
            thread::yield_now();
        }
        sent += progress;
    }

    Ok(())
}
