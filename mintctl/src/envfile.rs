//! Minimal .env reader/writer.
//!
//! Semantics match the bash originals exactly: `get` returns the LAST
//! occurrence of a key (sed | tail -1), `set` drops every existing line for
//! the key and appends one at the end (env_set), values are raw text after
//! the first `=` — no quoting or escaping, because docker compose reads the
//! same file with the same rules. Comments and unrelated lines are preserved.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub struct EnvFile {
    path: PathBuf,
    lines: Vec<String>,
}

impl EnvFile {
    pub fn load(path: &Path) -> Result<Self> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            lines: raw.lines().map(str::to_string).collect(),
        })
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let prefix = format!("{key}=");
        self.lines
            .iter()
            .rev()
            .find_map(|line| line.strip_prefix(&prefix))
            .map(str::to_string)
    }

    pub fn set(&mut self, key: &str, value: &str) {
        let prefix = format!("{key}=");
        self.lines.retain(|line| !line.starts_with(&prefix));
        self.lines.push(format!("{key}={value}"));
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.lines
            .iter()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key))
            .filter(|key| !key.starts_with('#'))
    }

    /// Write back via temp file + rename, preserving the original's mode.
    pub fn save(&self) -> Result<()> {
        let mode = std::fs::metadata(&self.path)
            .map(|m| std::os::unix::fs::MetadataExt::mode(&m))
            .unwrap_or(0o600);
        let mut body = self.lines.join("\n");
        body.push('\n');
        crate::storage::atomic_write(&self.path, body.as_bytes(), mode & 0o777)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_env(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        std::fs::write(&path, content).expect("write");
        (dir, path)
    }

    #[test]
    fn get_returns_last_occurrence_and_raw_value() {
        let (_dir, path) = temp_env("# comment\nKEY=first\nOTHER=x\nKEY=second=with=equals\n");
        let env = EnvFile::load(&path).expect("load");
        assert_eq!(env.get("KEY").as_deref(), Some("second=with=equals"));
        assert_eq!(env.get("OTHER").as_deref(), Some("x"));
        assert_eq!(env.get("MISSING"), None);
    }

    #[test]
    fn set_dedupes_appends_and_preserves_comments() {
        let (_dir, path) = temp_env("# header\nKEY=old\nOTHER=x\nKEY=older\n");
        let mut env = EnvFile::load(&path).expect("load");
        env.set("KEY", "new");
        env.save().expect("save");
        let raw = std::fs::read_to_string(&path).expect("read");
        assert_eq!(raw, "# header\nOTHER=x\nKEY=new\n");
    }

    #[test]
    fn save_preserves_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = temp_env("KEY=v\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let mut env = EnvFile::load(&path).expect("load");
        env.set("KEY", "w");
        env.save().expect("save");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
