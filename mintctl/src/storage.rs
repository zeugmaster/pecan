//! Private, atomic writes and a per-install advisory operation lock.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

pub fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("path has no parent")?)?;
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(mode))?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

pub fn lock(dir: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join(".mintctl.lock"))
        .with_context(|| format!("cannot manage {} as this user", dir.display()))?;
    file.try_lock().context(
        "another mintctl operation is running for this installation; retry when it finishes",
    )?;
    // Keep this inode in place; deleting the lock file would allow two locks.
    Ok(file)
}
