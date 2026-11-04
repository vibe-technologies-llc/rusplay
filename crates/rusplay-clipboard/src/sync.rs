use std::collections::VecDeque;

use rusplay_config::InputConfig;
use rusplay_protocol::{ClipboardChunk, ClipboardReassembler, ProtocolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicy {
    pub send: bool,
    pub receive: bool,
    pub max_bytes: usize,
}

impl ClipboardPolicy {
    pub fn host(input: &InputConfig) -> Self {
        Self {
            send: input.clipboard.host_sends(),
            receive: input.clipboard.client_sends(),
            max_bytes: Self::limit(input),
        }
    }

    pub fn client(input: &InputConfig) -> Self {
        Self {
            send: input.clipboard.client_sends(),
            receive: input.clipboard.host_sends(),
            max_bytes: Self::limit(input),
        }
    }

    pub const fn is_enabled(&self) -> bool {
        self.send || self.receive
    }

    fn limit(input: &InputConfig) -> usize {
        usize::try_from(input.clipboard_max_bytes).unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Queued,
    SendDisabled,
    Empty,
    TooLarge,
    AlreadySynced,
}

#[derive(Debug)]
pub struct ClipboardSync {
    policy: ClipboardPolicy,
    reassembler: ClipboardReassembler,
    last_synced: Option<String>,
    next_transfer: u32,
    outgoing: VecDeque<ClipboardChunk>,
}

impl ClipboardSync {
    pub fn new(policy: ClipboardPolicy) -> Self {
        Self {
            policy,
            reassembler: ClipboardReassembler::new(policy.max_bytes),
            last_synced: None,
            next_transfer: 0,
            outgoing: VecDeque::new(),
        }
    }

    pub const fn policy(&self) -> ClipboardPolicy {
        self.policy
    }

    pub fn local_changed(&mut self, text: String) -> Disposition {
        if !self.policy.send {
            return Disposition::SendDisabled;
        }
        if text.is_empty() {
            return Disposition::Empty;
        }
        if text.len() > self.policy.max_bytes {
            return Disposition::TooLarge;
        }
        if self.last_synced.as_deref() == Some(text.as_str()) {
            return Disposition::AlreadySynced;
        }

        let transfer = self.next_transfer;
        let Ok(chunks) = ClipboardChunk::split(transfer, &text) else {
            return Disposition::TooLarge;
        };

        self.next_transfer = transfer.wrapping_add(1);
        self.outgoing = chunks.into();
        self.last_synced = Some(text);

        Disposition::Queued
    }

    pub fn receive(&mut self, chunk: &ClipboardChunk) -> Result<Option<String>, ProtocolError> {
        if !self.policy.receive {
            return Ok(None);
        }

        let Some(text) = self.reassembler.push(chunk)? else {
            return Ok(None);
        };
        if self.last_synced.as_deref() == Some(text.as_str()) {
            return Ok(None);
        }

        self.last_synced = Some(text.clone());
        Ok(Some(text))
    }

    pub fn peek_chunk(&self) -> Option<&ClipboardChunk> {
        self.outgoing.front()
    }

    pub fn pop_chunk(&mut self) -> Option<ClipboardChunk> {
        self.outgoing.pop_front()
    }

    pub fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty()
    }

    pub fn reset(&mut self) {
        self.reassembler.reset();
        self.outgoing.clear();
        self.last_synced = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(send: bool, receive: bool) -> ClipboardPolicy {
        ClipboardPolicy {
            send,
            receive,
            max_bytes: 2000,
        }
    }

    fn drain(sync: &mut ClipboardSync) -> Vec<ClipboardChunk> {
        std::iter::from_fn(|| sync.pop_chunk()).collect()
    }

    fn deliver(sync: &mut ClipboardSync, chunks: &[ClipboardChunk]) -> Option<String> {
        chunks
            .iter()
            .filter_map(|chunk| sync.receive(chunk).unwrap())
            .last()
    }

    #[test]
    fn host_and_client_policies_mirror_the_direction() {
        use rusplay_config::ClipboardDirection::{Both, ClientToHost, HostToClient, Off};

        let cases = [
            (Off, false, false),
            (ClientToHost, false, true),
            (HostToClient, true, false),
            (Both, true, true),
        ];

        for (direction, host_sends, host_receives) in cases {
            let config = InputConfig {
                clipboard: direction,
                ..InputConfig::default()
            };
            let host = ClipboardPolicy::host(&config);
            let client = ClipboardPolicy::client(&config);

            assert_eq!((host.send, host.receive), (host_sends, host_receives));
            assert_eq!((client.send, client.receive), (host_receives, host_sends));
            assert_eq!(host.max_bytes, 65_536);
        }
    }

    #[test]
    fn a_local_change_travels_and_arrives_intact() {
        let mut sender = ClipboardSync::new(policy(true, false));
        let mut receiver = ClipboardSync::new(policy(false, true));
        let text = "héllo ".repeat(250);

        assert_eq!(sender.local_changed(text.clone()), Disposition::Queued);
        let chunks = drain(&mut sender);

        assert!(chunks.len() > 1);
        assert_eq!(deliver(&mut receiver, &chunks), Some(text));
    }

    #[test]
    fn nothing_is_sent_when_sending_is_disabled() {
        let mut sync = ClipboardSync::new(policy(false, true));

        assert_eq!(
            sync.local_changed("secret".to_owned()),
            Disposition::SendDisabled
        );
        assert!(!sync.has_outgoing());
    }

    #[test]
    fn nothing_is_applied_when_receiving_is_disabled() {
        let mut sender = ClipboardSync::new(policy(true, false));
        let mut receiver = ClipboardSync::new(policy(true, false));
        sender.local_changed("pushed at you".to_owned());

        let applied = deliver(&mut receiver, &drain(&mut sender));

        assert_eq!(applied, None);
    }

    #[test]
    fn oversized_and_empty_text_is_not_sent() {
        let mut sync = ClipboardSync::new(policy(true, true));

        assert_eq!(sync.local_changed("a".repeat(2001)), Disposition::TooLarge);
        assert_eq!(sync.local_changed(String::new()), Disposition::Empty);
        assert_eq!(sync.local_changed("a".repeat(2000)), Disposition::Queued);
    }

    #[test]
    fn a_received_message_over_the_limit_is_an_error_not_an_allocation() {
        let mut big = ClipboardSync::new(ClipboardPolicy {
            send: true,
            receive: false,
            max_bytes: 100_000,
        });
        let mut small = ClipboardSync::new(policy(false, true));
        big.local_changed("a".repeat(50_000));

        let first = big.pop_chunk().unwrap();

        assert!(matches!(
            small.receive(&first),
            Err(ProtocolError::ClipboardTooLarge { max: 2000, .. })
        ));
    }

    #[test]
    fn a_value_received_from_the_peer_is_not_echoed_back() {
        let mut host = ClipboardSync::new(policy(true, true));
        let mut client = ClipboardSync::new(policy(true, true));

        client.local_changed("copied on the client".to_owned());
        let applied = deliver(&mut host, &drain(&mut client));

        assert_eq!(applied.as_deref(), Some("copied on the client"));
        assert_eq!(
            host.local_changed("copied on the client".to_owned()),
            Disposition::AlreadySynced
        );
        assert!(!host.has_outgoing());
    }

    #[test]
    fn a_value_sent_is_not_resent_when_the_peer_echoes_it() {
        let mut host = ClipboardSync::new(policy(true, true));
        let mut client = ClipboardSync::new(policy(true, true));

        host.local_changed("from the host".to_owned());
        let host_chunks = drain(&mut host);
        deliver(&mut client, &host_chunks);

        assert_eq!(
            client.local_changed("from the host".to_owned()),
            Disposition::AlreadySynced
        );
        assert_eq!(deliver(&mut client, &host_chunks), None);
    }

    #[test]
    fn copying_an_older_value_again_is_sent_again() {
        let mut sync = ClipboardSync::new(policy(true, true));

        sync.local_changed("one".to_owned());
        sync.local_changed("two".to_owned());
        drain(&mut sync);

        assert_eq!(sync.local_changed("one".to_owned()), Disposition::Queued);
    }

    #[test]
    fn a_newer_local_change_replaces_chunks_not_yet_sent() {
        let mut sync = ClipboardSync::new(policy(true, true));

        sync.local_changed("a".repeat(1500));
        sync.pop_chunk();
        sync.local_changed("fresh".to_owned());
        let chunks = drain(&mut sync);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].data(), b"fresh");
    }

    #[test]
    fn reset_forgets_the_last_value_and_the_queue() {
        let mut sync = ClipboardSync::new(policy(true, true));
        sync.local_changed("one".to_owned());

        sync.reset();

        assert!(!sync.has_outgoing());
        assert_eq!(sync.local_changed("one".to_owned()), Disposition::Queued);
    }
}
