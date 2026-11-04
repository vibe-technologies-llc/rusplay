use std::{collections::VecDeque, sync::Arc};

use parking_lot::Mutex;

use crate::{BackendKind, ClipboardBackend, ClipboardError};

#[derive(Debug, Default)]
struct MemoryState {
    contents: Option<String>,
    changes: VecDeque<String>,
    writes: usize,
    polls: usize,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryClipboard {
    state: Arc<Mutex<MemoryState>>,
}

impl MemoryClipboard {
    pub fn copy(&self, text: impl Into<String>) {
        let text = text.into();
        let mut state = self.state.lock();

        state.contents = Some(text.clone());
        state.changes.push_back(text);
    }

    pub fn contents(&self) -> Option<String> {
        self.state.lock().contents.clone()
    }

    pub fn writes(&self) -> usize {
        self.state.lock().writes
    }

    pub fn polls(&self) -> usize {
        self.state.lock().polls
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn kind(&self) -> BackendKind {
        BackendKind::Memory
    }

    fn poll_change(&mut self) -> Result<Option<String>, ClipboardError> {
        let mut state = self.state.lock();

        state.polls += 1;
        Ok(state.changes.pop_front())
    }

    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        let mut state = self.state.lock();

        state.contents = Some(text.to_owned());
        state.changes.push_back(text.to_owned());
        state.writes += 1;
        Ok(())
    }
}
