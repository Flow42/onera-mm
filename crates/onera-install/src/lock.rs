//! Per-game-installation serialization.
//!
//! Two deployments into the same game directory at the same time would
//! interleave their renames and produce a provider stack that matches neither
//! plan. Deployments are therefore serialized *per game installation* — two
//! different games can install concurrently, which is what makes bulk
//! operations tolerable.

use onera_core::ids::LocalGameId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};

/// A registry of per-game locks.
#[derive(Debug, Default, Clone)]
pub struct GameLocks {
    locks: Arc<Mutex<HashMap<LocalGameId, Arc<Mutex<()>>>>>,
}

impl GameLocks {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the lock for one game installation, waiting if necessary.
    ///
    /// The returned guard must be held for the whole operation.
    pub async fn acquire(&self, game: LocalGameId) -> OwnedMutexGuard<()> {
        let lock = {
            let mut map = self.locks.lock().await;
            Arc::clone(map.entry(game).or_default())
        };
        lock.lock_owned().await
    }

    /// Whether a game is currently locked, for diagnostics.
    pub async fn is_locked(&self, game: LocalGameId) -> bool {
        let map = self.locks.lock().await;
        map.get(&game).is_some_and(|l| l.try_lock().is_err())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn two_installs_into_one_game_are_serialized() {
        let locks = GameLocks::new();
        let game = LocalGameId::new();
        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let (locks, concurrent, peak) = (locks.clone(), concurrent.clone(), peak.clone());
            handles.push(tokio::spawn(async move {
                let _guard = locks.acquire(game).await;
                let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                concurrent.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "deployments into one game overlapped"
        );
    }

    #[tokio::test]
    async fn different_games_do_not_block_each_other() {
        let locks = GameLocks::new();
        let first = locks.acquire(LocalGameId::new()).await;
        // A second game must not wait on the first.
        let second = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            locks.acquire(LocalGameId::new()),
        )
        .await;
        assert!(second.is_ok(), "an unrelated game was blocked");
        drop(first);
    }

    #[tokio::test]
    async fn lock_state_is_observable() {
        let locks = GameLocks::new();
        let game = LocalGameId::new();
        assert!(!locks.is_locked(game).await);
        let guard = locks.acquire(game).await;
        assert!(locks.is_locked(game).await);
        drop(guard);
        assert!(!locks.is_locked(game).await);
    }
}
