use rusplay_capture::{CaptureError, RestoreTokenStore};
use rusplay_config::Config;

use crate::HostError;

#[derive(Debug)]
pub(crate) struct RememberedDisplay {
    store: RestoreTokenStore,
    token: String,
}

impl RememberedDisplay {
    pub(crate) fn forget(config: &mut Config) -> Result<Option<Self>, HostError> {
        config.capture.restore_token = None;
        if !config.capture.persist_session {
            return Ok(None);
        }

        let store = RestoreTokenStore::from_config(&config.capture)?;
        let Some(token) = store.load()? else {
            return Ok(None);
        };
        store.clear()?;

        Ok(Some(Self { store, token }))
    }

    pub(crate) fn restore(self) -> Result<(), CaptureError> {
        self.store.store(&self.token)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn persisting(directory: &TempDir) -> (Config, RestoreTokenStore) {
        let path = directory.path().join("restore-token");
        let mut config = Config::default();
        config.capture.persist_session = true;
        config.capture.restore_token_file = Some(path.clone());

        (config, RestoreTokenStore::new(path))
    }

    #[test]
    fn forgetting_clears_the_stored_token_and_restoring_writes_it_back() {
        let directory = tempfile::tempdir().unwrap();
        let (mut config, store) = persisting(&directory);
        store.store("remembered").unwrap();

        let remembered = RememberedDisplay::forget(&mut config).unwrap().unwrap();
        let cleared = store.load().unwrap();
        remembered.restore().unwrap();

        assert_eq!(cleared, None);
        assert_eq!(store.load().unwrap().as_deref(), Some("remembered"));
    }

    #[test]
    fn forgetting_drops_a_token_given_in_the_config() {
        let directory = tempfile::tempdir().unwrap();
        let (mut config, _store) = persisting(&directory);
        config.capture.restore_token = Some("configured".to_owned());

        let remembered = RememberedDisplay::forget(&mut config).unwrap();

        assert!(remembered.is_none());
        assert_eq!(config.capture.restore_token, None);
    }

    #[test]
    fn without_persistence_the_store_is_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let (mut config, store) = persisting(&directory);
        store.store("remembered").unwrap();
        config.capture.persist_session = false;

        let remembered = RememberedDisplay::forget(&mut config).unwrap();

        assert!(remembered.is_none());
        assert_eq!(store.load().unwrap().as_deref(), Some("remembered"));
    }
}
