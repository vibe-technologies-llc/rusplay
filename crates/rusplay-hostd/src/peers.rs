use std::fmt::Write;

use rusplay_host::{PeerId, PeerRecord, PeerStore};
use thiserror::Error;

const MIN_ID_PREFIX_CHARS: usize = 4;

#[derive(Debug, Error)]
pub enum PeersError {
    #[error("no paired client matches {target:?}")]
    NotFound { target: String },
    #[error("{target:?} matches {count} paired clients, use more of the id")]
    Ambiguous { target: String, count: usize },
}

pub fn render_list(store: &PeerStore) -> String {
    if store.is_empty() {
        return "no paired clients\n".to_owned();
    }

    let mut text = String::new();
    for record in store.iter() {
        writeln!(text, "{} {}", record.id(), record.name()).ok();
    }

    text
}

pub fn resolve(store: &PeerStore, target: &str) -> Result<PeerId, PeersError> {
    let wanted_id = target.to_ascii_lowercase();
    let id_prefix = wanted_id.len() >= MIN_ID_PREFIX_CHARS;
    let matches: Vec<&PeerRecord> = store
        .iter()
        .filter(|record| {
            record.name() == target
                || (id_prefix && record.id().to_string().starts_with(&wanted_id))
        })
        .collect();

    match matches.as_slice() {
        [] => Err(PeersError::NotFound {
            target: target.to_owned(),
        }),
        [only] => Ok(only.id()),
        many => Err(PeersError::Ambiguous {
            target: target.to_owned(),
            count: many.len(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use rusplay_net::PeerSecret;

    use super::*;

    fn store_with(records: &[(&str, u8)]) -> (tempfile::TempDir, PeerStore) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PeerStore::open(directory.path().join("paired-clients")).unwrap();
        for (name, byte) in records {
            let record = PeerRecord::new(name, PeerSecret::from_bytes([*byte; 32])).unwrap();
            store.remember(record).unwrap();
        }

        (directory, store)
    }

    fn id_of(byte: u8) -> PeerId {
        PeerSecret::from_bytes([byte; 32]).id()
    }

    #[test]
    fn the_list_has_one_line_per_peer_and_never_a_secret() {
        let (_directory, store) = store_with(&[("laptop", 1), ("desk pc", 2)]);

        let text = render_list(&store);

        assert_eq!(text, format!("{} laptop\n{} desk pc\n", id_of(1), id_of(2)));
    }

    #[test]
    fn an_empty_store_says_so() {
        let (_directory, store) = store_with(&[]);

        assert_eq!(render_list(&store), "no paired clients\n");
    }

    #[test]
    fn a_name_a_full_id_and_an_id_prefix_all_resolve() {
        let (_directory, store) = store_with(&[("laptop", 1), ("desk pc", 2)]);
        let full = id_of(2).to_string();

        assert_eq!(resolve(&store, "laptop").unwrap(), id_of(1));
        assert_eq!(resolve(&store, &full).unwrap(), id_of(2));
        assert_eq!(resolve(&store, &full[..6]).unwrap(), id_of(2));
        assert_eq!(resolve(&store, &full.to_uppercase()).unwrap(), id_of(2));
    }

    #[test]
    fn unknown_short_and_ambiguous_targets_are_refused() {
        let (_directory, store) = store_with(&[("same", 1), ("same", 2)]);
        let first = id_of(1).to_string();

        assert!(matches!(
            resolve(&store, "other"),
            Err(PeersError::NotFound { .. })
        ));
        assert!(matches!(
            resolve(&store, &first[..MIN_ID_PREFIX_CHARS - 1]),
            Err(PeersError::NotFound { .. })
        ));
        assert!(matches!(
            resolve(&store, "same"),
            Err(PeersError::Ambiguous { count: 2, .. })
        ));
        assert_eq!(resolve(&store, &first).unwrap(), id_of(1));
    }
}
