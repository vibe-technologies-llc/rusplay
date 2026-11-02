use std::{net::SocketAddr, time::Instant};

use rusplay_protocol::{PeerId, ReconnectMessage};

use crate::{
    NetError, PeerStore, ReconnectOutput, ReconnectResponder, ReconnectTiming, busy::busy_reply,
    reconnect::is_blinded_by,
};

#[derive(Debug)]
pub struct SessionTakeover {
    peer: PeerId,
    responder: ReconnectResponder,
}

impl SessionTakeover {
    pub fn new(peer: PeerId, timing: ReconnectTiming) -> Result<Self, NetError> {
        Ok(Self {
            peer,
            responder: ReconnectResponder::new(timing)?,
        })
    }

    pub const fn peer(&self) -> PeerId {
        self.peer
    }

    pub fn handle(
        &mut self,
        datagram: &[u8],
        from: SocketAddr,
        store: &PeerStore,
    ) -> Result<ReconnectOutput, NetError> {
        let message = ReconnectMessage::from_datagram(datagram)?;
        let session = store.get(&self.peer);
        let from_session_peer = session.is_some_and(|record| is_blinded_by(record, &message));

        match message {
            ReconnectMessage::Hello { .. } if !from_session_peer => Ok(ReconnectOutput {
                reply: busy_reply(datagram),
                accepted: None,
            }),
            ReconnectMessage::Hello { .. } | ReconnectMessage::Proof { .. } => self
                .responder
                .respond(&message, from, session, Instant::now()),
            _ => Ok(ReconnectOutput::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use rusplay_protocol::ReconnectRejection;

    use super::*;
    use crate::{PEER_SECRET_LEN, PeerRecord, PeerSecret, ReconnectInitiator, ReconnectStep};

    const OLD: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4000);
    const NEW: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5000);
    const OTHER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6000);

    fn record(name: &str, byte: u8) -> PeerRecord {
        PeerRecord::new(name, PeerSecret::from_bytes([byte; PEER_SECRET_LEN])).unwrap()
    }

    fn store(directory: &tempfile::TempDir, records: &[PeerRecord]) -> PeerStore {
        let mut store = PeerStore::open(directory.path().join("clients")).unwrap();
        for record in records {
            store.remember(record.clone()).unwrap();
        }
        store
    }

    fn rejection_of(datagram: &[u8]) -> ReconnectRejection {
        let Ok(ReconnectMessage::Rejected { reason, .. }) =
            ReconnectMessage::from_datagram(datagram)
        else {
            panic!("expected a rejection");
        };
        reason
    }

    fn challenged(
        takeover: &mut SessionTakeover,
        initiator: &mut ReconnectInitiator,
        store: &PeerStore,
    ) {
        let hello = initiator.outgoing().unwrap().to_vec();
        let challenge = takeover.handle(&hello, NEW, store).unwrap().reply.unwrap();

        assert!(matches!(
            initiator.handle(&challenge),
            Ok(ReconnectStep::Pending)
        ));
    }

    #[test]
    fn the_session_peer_takes_over_after_proving_the_secret() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, std::slice::from_ref(&laptop));
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();

        challenged(&mut takeover, &mut initiator, &store);
        let proof = initiator.outgoing().unwrap().to_vec();
        let output = takeover.handle(&proof, NEW, &store).unwrap();
        let accepted = output.accepted.unwrap();

        assert_eq!(accepted.peer, laptop.id());
        assert!(matches!(
            initiator.handle(&output.reply.unwrap()),
            Ok(ReconnectStep::Complete(_))
        ));
    }

    #[test]
    fn a_forged_proof_is_rejected_and_admits_nobody() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, std::slice::from_ref(&laptop));
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        challenged(&mut takeover, &mut initiator, &store);

        let mut forged = initiator.outgoing().unwrap().to_vec();
        let last = forged.len() - 1;
        forged[last] ^= 0xFF;
        let output = takeover.handle(&forged, NEW, &store).unwrap();

        assert!(output.accepted.is_none());
        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::Authentication
        );
    }

    #[test]
    fn a_genuine_proof_from_another_address_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, std::slice::from_ref(&laptop));
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        challenged(&mut takeover, &mut initiator, &store);

        let proof = initiator.outgoing().unwrap().to_vec();
        let replayed = takeover.handle(&proof, OTHER, &store).unwrap();
        let genuine = takeover.handle(&proof, NEW, &store).unwrap();

        assert!(replayed.reply.is_none());
        assert!(replayed.accepted.is_none());
        assert!(genuine.accepted.is_some());
    }

    #[test]
    fn a_proof_for_an_attempt_that_never_said_hello_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, std::slice::from_ref(&laptop));
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();

        let stray = ReconnectMessage::Proof {
            attempt: 5,
            nonce: [1; ReconnectMessage::NONCE_LEN],
            blind: [1; ReconnectMessage::BLIND_LEN],
            cookie: [1; ReconnectMessage::COOKIE_LEN],
            proof: [1; ReconnectMessage::PROOF_LEN],
        }
        .to_datagram()
        .unwrap();
        let output = takeover.handle(&stray, OLD, &store).unwrap();

        assert!(output.reply.is_none());
        assert!(output.accepted.is_none());
    }

    #[test]
    fn another_peer_is_told_the_host_is_busy_even_when_known() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let desktop = record("desktop", 2);
        let store = store(&directory, &[laptop.clone(), desktop.clone()]);
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();
        let initiator = ReconnectInitiator::new(&desktop).unwrap();

        let hello = initiator.outgoing().unwrap().to_vec();
        let output = takeover.handle(&hello, NEW, &store).unwrap();

        assert!(output.accepted.is_none());
        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::Busy
        );
        assert!(initiator.outgoing().is_some());
    }

    #[test]
    fn a_flood_of_spoofed_hellos_does_not_block_the_session_peer() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, std::slice::from_ref(&laptop));
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        challenged(&mut takeover, &mut initiator, &store);
        let captured = ReconnectInitiator::new(&laptop).unwrap();

        for port in 1..=2048_u16 {
            let spoofed = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 9, 0, 1)), port);
            let stranger = ReconnectInitiator::new(&record("stranger", 7)).unwrap();
            takeover
                .handle(stranger.outgoing().unwrap(), spoofed, &store)
                .unwrap();
            takeover
                .handle(captured.outgoing().unwrap(), spoofed, &store)
                .unwrap();
        }
        let proof = initiator.outgoing().unwrap().to_vec();
        let output = takeover.handle(&proof, NEW, &store).unwrap();

        assert!(output.accepted.is_some());
    }

    #[test]
    fn a_stranger_cannot_pose_as_the_session_peer() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, std::slice::from_ref(&laptop));
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();
        let impostor = ReconnectInitiator::new(&record("laptop", 2)).unwrap();

        let output = takeover
            .handle(impostor.outgoing().unwrap(), NEW, &store)
            .unwrap();

        assert!(output.accepted.is_none());
        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::Busy
        );
    }

    #[test]
    fn a_forgotten_session_peer_is_no_longer_recognised() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, &[]);
        let mut takeover = SessionTakeover::new(laptop.id(), ReconnectTiming::default()).unwrap();
        let initiator = ReconnectInitiator::new(&laptop).unwrap();

        let output = takeover
            .handle(initiator.outgoing().unwrap(), NEW, &store)
            .unwrap();

        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::Busy
        );
    }
}
