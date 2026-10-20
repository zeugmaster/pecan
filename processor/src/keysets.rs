//! What this processor has observed about the attached mint's keysets.
//!
//! `/v1/keysets` says which keysets exist, which one is active, and when each
//! one's ecash stops being redeemable — but not when a keyset started or
//! stopped issuing. The console's key timeline needs both, so the processor
//! records when it first saw each keyset and when it saw an active one go
//! inactive. Keysets that already existed the first time the processor read
//! a mint's list started some unknown time before that first sighting.
//!
//! Observations are a convenience, not a ledger: losing `keysets.json` only
//! makes every current keyset's start "unknown" again.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::clients::KeysetEntry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeysetSighting {
    /// When this processor first saw the keyset in the mint's list.
    pub first_seen_at: u64,
    /// False when the keyset was already listed at the first look at this
    /// mint: it started at some unknown time before `first_seen_at`.
    pub start_observed: bool,
    /// The keyset's `active` flag at the last observation.
    pub active: bool,
    /// When a keyset previously seen active was first seen inactive.
    pub deactivated_at: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LogFile {
    #[serde(default)]
    keysets: BTreeMap<String, KeysetSighting>,
}

#[derive(Clone)]
pub struct KeysetLog {
    inner: Arc<Inner>,
}

struct Inner {
    sightings: RwLock<BTreeMap<String, KeysetSighting>>,
    path: PathBuf,
}

impl KeysetLog {
    pub async fn load(path: PathBuf) -> Result<Self> {
        let sightings = if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let raw = tokio::fs::read(&path)
                .await
                .with_context(|| format!("read {}", path.display()))?;
            match serde_json::from_slice::<LogFile>(&raw) {
                Ok(file) => file.keysets,
                Err(e) => {
                    tracing::warn!(
                        "{} is unreadable ({e}); keyset start times restart as unknown",
                        path.display()
                    );
                    BTreeMap::new()
                }
            }
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            inner: Arc::new(Inner {
                sightings: RwLock::new(sightings),
                path,
            }),
        })
    }

    /// Record one read of the mint's keyset list. Returns whether anything
    /// new was learned (and persisted).
    pub async fn observe(&self, keysets: &[KeysetEntry], now: u64) -> bool {
        let changed = apply_observation(&mut *self.inner.sightings.write().await, keysets, now);
        if changed {
            if let Err(e) = self.persist().await {
                tracing::warn!("persist keyset observations: {e:#}");
            }
        }
        changed
    }

    pub async fn sightings(&self) -> BTreeMap<String, KeysetSighting> {
        self.inner.sightings.read().await.clone()
    }

    async fn persist(&self) -> Result<()> {
        let keysets = self.inner.sightings.read().await.clone();
        let bytes = serde_json::to_vec_pretty(&LogFile { keysets })?;
        let tmp = self.inner.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, &self.inner.path).await?;
        Ok(())
    }
}

fn apply_observation(
    sightings: &mut BTreeMap<String, KeysetSighting>,
    keysets: &[KeysetEntry],
    now: u64,
) -> bool {
    if keysets.is_empty() {
        return false;
    }
    // A list sharing no id with what was recorded is a first look — at this
    // mint, or at a different one after the attachment changed. Everything
    // in it predates the observation. (A changed mint URL alone keeps the
    // history: the same mint lists the same ids.)
    let first_look = !keysets.iter().any(|k| sightings.contains_key(&k.id));
    let mut changed = false;
    if first_look && !sightings.is_empty() {
        sightings.clear();
        changed = true;
    }
    for keyset in keysets {
        match sightings.get_mut(&keyset.id) {
            None => {
                sightings.insert(
                    keyset.id.clone(),
                    KeysetSighting {
                        first_seen_at: now,
                        start_observed: !first_look,
                        active: keyset.active,
                        deactivated_at: None,
                    },
                );
                changed = true;
            }
            Some(sighting) if sighting.active != keyset.active => {
                sighting.deactivated_at = if keyset.active { None } else { Some(now) };
                sighting.active = keyset.active;
                changed = true;
            }
            Some(_) => {}
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyset(id: &str, active: bool) -> KeysetEntry {
        KeysetEntry {
            id: id.into(),
            unit: "ora".into(),
            active,
            input_fee_ppk: 0,
            final_expiry: None,
        }
    }

    #[test]
    fn first_look_marks_starts_unknown_and_later_keysets_observed() {
        let mut log = BTreeMap::new();
        assert!(apply_observation(&mut log, &[keyset("a", true)], 100));
        assert!(!log["a"].start_observed);
        assert_eq!(log["a"].first_seen_at, 100);

        // Nothing new: no write.
        assert!(!apply_observation(&mut log, &[keyset("a", true)], 200));

        // A rotation: the old keyset stops issuing, the new one is seen start.
        assert!(apply_observation(
            &mut log,
            &[keyset("a", false), keyset("b", true)],
            300
        ));
        assert_eq!(log["a"].deactivated_at, Some(300));
        assert!(log["b"].start_observed);
        assert_eq!(log["b"].first_seen_at, 300);
        assert_eq!(log["b"].deactivated_at, None);
    }

    #[test]
    fn an_unrelated_list_starts_a_fresh_log() {
        let mut log = BTreeMap::new();
        apply_observation(&mut log, &[keyset("a", false), keyset("b", true)], 100);
        assert!(apply_observation(&mut log, &[keyset("x", true)], 500));
        assert_eq!(log.len(), 1);
        assert!(!log["x"].start_observed);
    }

    #[test]
    fn empty_lists_change_nothing() {
        let mut log = BTreeMap::new();
        apply_observation(&mut log, &[keyset("a", true)], 100);
        assert!(!apply_observation(&mut log, &[], 200));
        assert_eq!(log.len(), 1);
    }
}
