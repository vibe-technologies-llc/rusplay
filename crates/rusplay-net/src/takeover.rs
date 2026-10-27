use std::{collections::VecDeque, net::SocketAddr};

use rusplay_protocol::{PeerId, ReconnectMessage};

use crate::{NetError, PeerStore, ReconnectOutput, ReconnectResponder, busy::busy_reply};

const MAX_TRACKED_ORIGINS: usize = 16;

#[derive(Debug)]
pub struct SessionTakeover {
    peer: PeerId,
    responder: ReconnectResponder,
    origins: VecDeque<(u32, SocketAddr)>,
}

impl SessionTakeover {
    pub fn new(peer: PeerId) -> Self {
        Self {
            peer,
            responder: ReconnectResponder::new(),
            origins: VecDeque::new(),
        }
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
        match ReconnectMessage::from_datagram(datagram)? {
            ReconnectMessage::Hello { attempt, peer, .. } if peer == self.peer => {
                self.remember_origin(attempt, from);
                self.responder.handle(datagram, store)
            }
            ReconnectMessage::Hello { .. } => Ok(ReconnectOutput {
                reply: busy_reply(datagram),
                accepted: None,
            }),
            ReconnectMessage::Proof { attempt, .. } if self.origin_of(attempt) == Some(from) => {
                self.responder.handle(datagram, store)
            }
            _ => Ok(ReconnectOutput::default()),
        }
    }

    fn remember_origin(&mut self, attempt: u32, from: SocketAddr) {
        if self.origin_of(attempt).is_some() {
            return;
        }
        if self.origins.len() >= MAX_TRACKED_ORIGINS {
            self.origins.pop_front();
        }
        self.origins.push_back((attempt, from));
    }

    fn origin_of(&self, attempt: u32) -> Option<SocketAddr> {
        self.origins
            .iter()
            .find(|(tracked, _)| *tracked == attempt)
            .map(|(_, address)| *address)
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
        let mut takeover = SessionTakeover::new(laptop.id());
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
        let mut takeover = SessionTakeover::new(laptop.id());
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        challenged(&mut takeover, &mut initiator, &store);

        let forged = ReconnectMessage::Proof {
            attempt: initiator.attempt(),
            proof: [9; ReconnectMessage::PROOF_LEN],
        }
        .to_datagram()
        .unwrap();
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
        let mut takeover = SessionTakeover::new(laptop.id());
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
        let mut takeover = SessionTakeover::new(laptop.id());

        let stray = ReconnectMessage::Proof {
            attempt: 5,
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
        let mut takeover = SessionTakeover::new(laptop.id());
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
    fn a_forgotten_session_peer_is_rejected_as_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store(&directory, &[]);
        let mut takeover = SessionTakeover::new(laptop.id());
        let initiator = ReconnectInitiator::new(&laptop).unwrap();

        let output = takeover
            .handle(initiator.outgoing().unwrap(), NEW, &store)
            .unwrap();

        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::UnknownPeer
        );
    }
}
