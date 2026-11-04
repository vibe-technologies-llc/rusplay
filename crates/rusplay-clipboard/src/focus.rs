use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crate::{BackendKind, ClipboardBackend, ClipboardError, ClipboardPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Sent,
    SendDisabled,
    Empty,
    TooLarge,
    Unchanged,
    Closed,
}

#[derive(Debug)]
pub struct FocusClipboard {
    outgoing: Receiver<String>,
    incoming: Sender<String>,
}

#[derive(Debug)]
pub struct FocusClipboardHandle {
    policy: ClipboardPolicy,
    outgoing: Sender<String>,
    incoming: Receiver<String>,
    last_seen: Option<String>,
}

impl FocusClipboard {
    pub fn pair(policy: ClipboardPolicy) -> (FocusClipboardHandle, Self) {
        let (outgoing_sender, outgoing) = mpsc::channel();
        let (incoming, incoming_receiver) = mpsc::channel();

        let handle = FocusClipboardHandle {
            policy,
            outgoing: outgoing_sender,
            incoming: incoming_receiver,
            last_seen: None,
        };

        (handle, Self { outgoing, incoming })
    }
}

impl ClipboardBackend for FocusClipboard {
    fn kind(&self) -> BackendKind {
        BackendKind::Focus
    }

    fn poll_change(&mut self) -> Result<Option<String>, ClipboardError> {
        match self.outgoing.try_recv() {
            Ok(text) => Ok(Some(text)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ClipboardError::BackendStopped),
        }
    }

    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.incoming
            .send(text.to_owned())
            .map_err(|_| ClipboardError::BackendStopped)
    }
}

impl FocusClipboardHandle {
    pub const fn reads_on_focus(&self) -> bool {
        self.policy.send
    }

    pub fn offer(&mut self, text: String) -> Offer {
        if !self.policy.send {
            return Offer::SendDisabled;
        }
        if text.is_empty() {
            return Offer::Empty;
        }
        if text.len() > self.policy.max_bytes {
            return Offer::TooLarge;
        }
        if self.last_seen.as_deref() == Some(text.as_str()) {
            return Offer::Unchanged;
        }

        self.last_seen = Some(text.clone());

        match self.outgoing.send(text) {
            Ok(()) => Offer::Sent,
            Err(_) => Offer::Closed,
        }
    }

    pub fn take_incoming(&mut self) -> Option<String> {
        let latest = self.incoming.try_iter().last()?;

        self.last_seen = Some(latest.clone());
        Some(latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClipboardLink, ClipboardSync};

    fn policy(send: bool, receive: bool) -> ClipboardPolicy {
        ClipboardPolicy {
            send,
            receive,
            max_bytes: 100,
        }
    }

    fn pair(send: bool, receive: bool) -> (FocusClipboardHandle, FocusClipboard) {
        FocusClipboard::pair(policy(send, receive))
    }

    #[test]
    fn a_changed_text_reaches_the_backend_once() {
        let (mut handle, mut backend) = pair(true, true);

        assert_eq!(handle.offer("copied".to_owned()), Offer::Sent);
        assert_eq!(backend.poll_change().unwrap().as_deref(), Some("copied"));
        assert_eq!(backend.poll_change().unwrap(), None);
    }

    #[test]
    fn an_unchanged_text_is_not_offered_again() {
        let (mut handle, mut backend) = pair(true, true);

        assert_eq!(handle.offer("copied".to_owned()), Offer::Sent);
        assert_eq!(handle.offer("copied".to_owned()), Offer::Unchanged);
        assert_eq!(handle.offer("other".to_owned()), Offer::Sent);

        assert_eq!(backend.poll_change().unwrap().as_deref(), Some("copied"));
        assert_eq!(backend.poll_change().unwrap().as_deref(), Some("other"));
        assert_eq!(backend.poll_change().unwrap(), None);
    }

    #[test]
    fn a_disabled_direction_neither_reads_nor_offers() {
        let (mut handle, mut backend) = pair(false, true);

        assert!(!handle.reads_on_focus());
        assert_eq!(handle.offer("copied".to_owned()), Offer::SendDisabled);
        assert_eq!(backend.poll_change().unwrap(), None);
    }

    #[test]
    fn empty_and_oversized_texts_are_dropped() {
        let (mut handle, mut backend) = pair(true, true);

        assert_eq!(handle.offer(String::new()), Offer::Empty);
        assert_eq!(handle.offer("a".repeat(101)), Offer::TooLarge);
        assert_eq!(handle.offer("a".repeat(100)), Offer::Sent);

        assert_eq!(backend.poll_change().unwrap(), Some("a".repeat(100)));
        assert_eq!(backend.poll_change().unwrap(), None);
    }

    #[test]
    fn incoming_text_reaches_the_handle_and_only_the_latest_is_kept() {
        let (mut handle, mut backend) = pair(true, true);

        backend.set_text("first").unwrap();
        backend.set_text("second").unwrap();

        assert_eq!(handle.take_incoming().as_deref(), Some("second"));
        assert_eq!(handle.take_incoming(), None);
    }

    #[test]
    fn text_applied_from_the_peer_is_not_offered_back_but_a_new_copy_is() {
        let (mut handle, mut backend) = pair(true, true);

        assert_eq!(handle.offer("mine".to_owned()), Offer::Sent);
        backend.set_text("theirs").unwrap();
        handle.take_incoming();

        assert_eq!(handle.offer("theirs".to_owned()), Offer::Unchanged);
        assert_eq!(handle.offer("mine".to_owned()), Offer::Sent);
    }

    #[test]
    fn a_dropped_handle_stops_the_backend() {
        let (handle, mut backend) = pair(true, true);

        drop(handle);

        assert!(matches!(
            backend.poll_change(),
            Err(ClipboardError::BackendStopped)
        ));
        assert!(matches!(
            backend.set_text("late"),
            Err(ClipboardError::BackendStopped)
        ));
    }

    #[test]
    fn a_dropped_backend_closes_the_offer() {
        let (mut handle, backend) = pair(true, true);

        drop(backend);

        assert_eq!(handle.offer("copied".to_owned()), Offer::Closed);
    }

    #[test]
    fn through_a_link_the_echo_of_an_applied_text_is_suppressed() {
        let client_policy = policy(true, true);
        let (mut handle, backend) = FocusClipboard::pair(client_policy);
        let mut link = ClipboardLink::new(client_policy, Box::new(backend));
        let mut host = ClipboardSync::new(policy(true, true));

        host.local_changed("from host".to_owned());
        while let Some(chunk) = host.pop_chunk() {
            link.receive(&chunk).unwrap();
        }
        let applied = handle.take_incoming();

        assert_eq!(applied.as_deref(), Some("from host"));
        assert_eq!(handle.offer("from host".to_owned()), Offer::Unchanged);

        let mut sent = 0;
        link.service(|_| {
            sent += 1;
            true
        })
        .unwrap();

        assert_eq!(sent, 0);
    }

    #[test]
    fn a_copy_through_a_link_is_sent_in_chunks() {
        let client_policy = policy(true, false);
        let (mut handle, backend) = FocusClipboard::pair(client_policy);
        let mut link = ClipboardLink::new(client_policy, Box::new(backend));

        assert_eq!(handle.offer("copied".to_owned()), Offer::Sent);

        let mut sent = 0;
        link.service(|_| {
            sent += 1;
            true
        })
        .unwrap();

        assert_eq!(sent, 1);
    }
}
