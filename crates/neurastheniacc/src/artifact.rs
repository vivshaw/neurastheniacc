//! Publish complete artifacts without exposing partially written files.
use anyhow::Result;
use std::{io::Write, path::Path};

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // The same directory keeps the final rename on one filesystem.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents.as_ref())?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn replaces_complete_files_and_cleans_up_failed_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.wasm");
        write(&path, b"old").unwrap();
        write(&path, b"complete replacement").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete replacement");
        let blocked = dir.path().join("directory");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("keep"), b"untouched").unwrap();
        assert!(write(&blocked, b"cannot replace directory").is_err());
        assert_eq!(fs::read(blocked.join("keep")).unwrap(), b"untouched");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
