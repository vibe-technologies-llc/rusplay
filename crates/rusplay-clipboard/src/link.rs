use rusplay_protocol::ClipboardChunk;

use crate::{BackendKind, ClipboardBackend, ClipboardError, ClipboardPolicy, ClipboardSync};

pub struct ClipboardLink {
    sync: ClipboardSync,
    backend: Box<dyn ClipboardBackend>,
}

impl std::fmt::Debug for ClipboardLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClipboardLink")
            .field("backend", &self.backend.kind())
            .field("policy", &self.sync.policy())
            .finish_non_exhaustive()
    }
}

impl ClipboardLink {
    pub fn new(policy: ClipboardPolicy, backend: Box<dyn ClipboardBackend>) -> Self {
        Self {
            sync: ClipboardSync::new(policy),
            backend,
        }
    }

    pub fn kind(&self) -> BackendKind {
        self.backend.kind()
    }

    pub fn has_pending(&self) -> bool {
        self.sync.has_outgoing()
    }

    pub fn reset(&mut self) {
        self.sync.reset();
    }

    pub fn receive(&mut self, chunk: &ClipboardChunk) -> Result<(), ClipboardError> {
        if let Some(text) = self.sync.receive(chunk)? {
            self.backend.set_text(&text)?;
        }

        Ok(())
    }

    pub fn service(
        &mut self,
        mut send: impl FnMut(ClipboardChunk) -> bool,
    ) -> Result<(), ClipboardError> {
        if self.sync.policy().send {
            while let Some(text) = self.backend.poll_change()? {
                self.sync.local_changed(text);
            }
        }

        while let Some(chunk) = self.sync.peek_chunk().copied() {
            if !send(chunk) {
                break;
            }
            self.sync.pop_chunk();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryClipboard;

    fn policy(send: bool, receive: bool) -> ClipboardPolicy {
        ClipboardPolicy {
            send,
            receive,
            max_bytes: 10_000,
        }
    }

    fn link(clipboard: &MemoryClipboard, send: bool, receive: bool) -> ClipboardLink {
        ClipboardLink::new(policy(send, receive), Box::new(clipboard.clone()))
    }

    fn carry(from: &mut ClipboardLink, to: &mut ClipboardLink) {
        let mut chunks = Vec::new();
        from.service(|chunk| {
            chunks.push(chunk);
            true
        })
        .unwrap();
        for chunk in &chunks {
            to.receive(chunk).unwrap();
        }
    }

    #[test]
    fn a_copy_on_one_side_lands_in_the_other_clipboard_and_stops() {
        let host_board = MemoryClipboard::default();
        let client_board = MemoryClipboard::default();
        let mut host = link(&host_board, true, true);
        let mut client = link(&client_board, true, true);

        client_board.copy("shared text");
        carry(&mut client, &mut host);
        carry(&mut host, &mut client);
        carry(&mut client, &mut host);

        assert_eq!(host_board.contents().as_deref(), Some("shared text"));
        assert_eq!(client_board.contents().as_deref(), Some("shared text"));
        assert_eq!(host_board.writes(), 1);
        assert_eq!(client_board.writes(), 0);
    }

    #[test]
    fn a_blocked_send_keeps_the_chunks_for_the_next_service() {
        let board = MemoryClipboard::default();
        let mut link = link(&board, true, false);

        board.copy("a".repeat(2000));
        let mut accepted = 0;
        link.service(|_| {
            accepted += 1;
            accepted <= 2
        })
        .unwrap();

        assert!(link.has_pending());

        let mut rest = 0;
        link.service(|_| {
            rest += 1;
            true
        })
        .unwrap();

        assert_eq!(accepted, 3);
        assert_eq!(
            accepted - 1 + rest,
            2000_usize.div_ceil(ClipboardChunk::MAX_DATA)
        );
        assert!(!link.has_pending());
    }

    #[test]
    fn a_receive_only_link_never_reads_the_local_clipboard() {
        let board = MemoryClipboard::default();
        let mut link = link(&board, false, true);

        board.copy("private");
        link.service(|_| panic!("nothing may be sent")).unwrap();

        assert_eq!(board.polls(), 0);
    }
}
