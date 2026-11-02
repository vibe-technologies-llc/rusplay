use std::{fs, io, path::Path};

use rusplay_protocol::HostId;

use crate::{NetError, pairing::fill_random, peers::write_owner_only};

pub fn load_or_create_host_id(path: &Path) -> Result<HostId, NetError> {
    match fs::read_to_string(path) {
        Ok(text) => HostId::from_hex(text.trim())
            .filter(|id| !id.is_none())
            .ok_or_else(|| NetError::HostIdCorrupt {
                path: path.to_owned(),
            }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => create(path),
        Err(source) => Err(NetError::HostIdRead {
            path: path.to_owned(),
            source,
        }),
    }
}

fn create(path: &Path) -> Result<HostId, NetError> {
    let mut bytes = [0; HostId::LEN];
    fill_random(&mut bytes)?;
    let id = HostId(bytes);

    write_owner_only(path, &format!("{id}\n")).map_err(|source| NetError::HostIdWrite {
        path: path.to_owned(),
        source,
    })?;

    Ok(id)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn the_first_call_creates_a_private_id_and_later_calls_return_it() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state").join("host-id");

        let first = load_or_create_host_id(&path).unwrap();
        let second = load_or_create_host_id(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;

        assert_eq!(first, second);
        assert!(!first.is_none());
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn two_hosts_get_different_ids() {
        let directory = TempDir::new().unwrap();

        let first = load_or_create_host_id(&directory.path().join("a")).unwrap();
        let second = load_or_create_host_id(&directory.path().join("b")).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn a_damaged_file_is_an_error_and_not_a_new_id() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("host-id");
        fs::write(&path, "not an id").unwrap();

        assert!(matches!(
            load_or_create_host_id(&path),
            Err(NetError::HostIdCorrupt { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not an id");
    }

    #[test]
    fn an_all_zero_id_is_refused() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("host-id");
        fs::write(&path, format!("{}\n", HostId::NONE)).unwrap();

        assert!(matches!(
            load_or_create_host_id(&path),
            Err(NetError::HostIdCorrupt { .. })
        ));
    }
}
