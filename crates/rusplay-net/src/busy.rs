use rusplay_protocol::{
    PacketHeader, PairingMessage, PairingRejection, ReconnectMessage, ReconnectRejection,
    StreamKind,
};

pub fn busy_reply(datagram: &[u8]) -> Option<Vec<u8>> {
    match PacketHeader::read(datagram).ok()?.stream {
        StreamKind::Pairing => match PairingMessage::from_datagram(datagram).ok()? {
            PairingMessage::Start { attempt, .. } => PairingMessage::Rejected {
                attempt,
                reason: PairingRejection::Busy,
            }
            .to_datagram()
            .ok(),
            _ => None,
        },
        StreamKind::Reconnect => match ReconnectMessage::from_datagram(datagram).ok()? {
            ReconnectMessage::Hello { attempt, .. } => ReconnectMessage::Rejected {
                attempt,
                reason: ReconnectRejection::Busy,
            }
            .to_datagram()
            .ok(),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rusplay_protocol::PeerId;

    use super::*;

    #[test]
    fn a_pairing_start_is_refused_with_busy_under_its_attempt() {
        let start = PairingMessage::Start {
            attempt: 7,
            spake: [1; PairingMessage::SPAKE_LEN],
        }
        .to_datagram()
        .unwrap();

        let reply = busy_reply(&start).unwrap();

        assert_eq!(
            PairingMessage::from_datagram(&reply),
            Ok(PairingMessage::Rejected {
                attempt: 7,
                reason: PairingRejection::Busy,
            })
        );
    }

    #[test]
    fn a_reconnect_hello_is_refused_with_busy_under_its_attempt() {
        let hello = ReconnectMessage::Hello {
            attempt: 9,
            peer: PeerId([3; PeerId::LEN]),
            nonce: [4; ReconnectMessage::NONCE_LEN],
        }
        .to_datagram()
        .unwrap();

        let reply = busy_reply(&hello).unwrap();

        assert_eq!(
            ReconnectMessage::from_datagram(&reply),
            Ok(ReconnectMessage::Rejected {
                attempt: 9,
                reason: ReconnectRejection::Busy,
            })
        );
    }

    #[test]
    fn replies_and_later_handshake_messages_get_no_answer() {
        let silent = [
            PairingMessage::Confirm {
                attempt: 1,
                confirm: [2; PairingMessage::MAC_LEN],
            }
            .to_datagram()
            .unwrap(),
            PairingMessage::Rejected {
                attempt: 1,
                reason: PairingRejection::Busy,
            }
            .to_datagram()
            .unwrap(),
            ReconnectMessage::Proof {
                attempt: 1,
                proof: [2; ReconnectMessage::PROOF_LEN],
            }
            .to_datagram()
            .unwrap(),
            ReconnectMessage::Rejected {
                attempt: 1,
                reason: ReconnectRejection::Busy,
            }
            .to_datagram()
            .unwrap(),
            vec![1, 2, 3],
        ];

        for datagram in silent {
            assert_eq!(busy_reply(&datagram), None);
        }
    }
}
