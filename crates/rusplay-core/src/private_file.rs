use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

pub fn write_owner_only(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let staging = path.with_added_extension("new");
    match fs::remove_file(&staging) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => return Err(source),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staging)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&staging, path)
}

pub fn read_regular(path: &Path) -> io::Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    #[test]
    fn planted_staging_files_and_symlinks_cannot_change_the_mode_or_target() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.txt");
        let staging = path.with_added_extension("new");
        fs::write(&staging, "public").unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o666)).unwrap();
        write_owner_only(&path, "secret").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        symlink(&path, &staging).unwrap();
        write_owner_only(&path, "replacement").unwrap();
        assert_eq!(read_regular(&path).unwrap(), "replacement");
    }

    #[test]
    fn reading_a_symlink_or_directory_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secret");
        let link = directory.path().join("link");
        write_owner_only(&path, "secret").unwrap();
        symlink(&path, &link).unwrap();
        assert!(read_regular(&link).is_err());
        assert!(read_regular(directory.path()).is_err());
    }
}
