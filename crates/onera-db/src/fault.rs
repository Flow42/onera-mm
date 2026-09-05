//! A persistence layer that fails on demand.
//!
//! `docs/recovery.md` claims that a database failure mid-operation is safe
//! because the journal write always precedes the filesystem effect it
//! describes: the next launch sees an earlier state and can roll back from it.
//! Until now that claim rested on reading the code rather than on a test, and
//! `docs/test-strategy.md` listed it as a known gap.
//!
//! This wraps a real [`Database`] and fails the Nth call to one chosen
//! operation, so the claim can be exercised. It mirrors
//! `onera_install::fs::fault` deliberately: same shape, same counting, so a
//! test can inject a filesystem fault and a database fault the same way.
//!
//! Public rather than `#[cfg(test)]` because the interesting failures — a
//! journal transition that dies between the rename and the record, a profile
//! activation that dies after the files are in place — are only reachable from
//! an integration test in another crate.

use crate::Database;
use async_trait::async_trait;
use onera_core::domain::operation::{Operation, OperationKind, OperationState};
use onera_core::domain::profile::{Profile, ProfileActivation, ProfileMember};
use onera_core::domain::reconcile::MutationPlan;
use onera_core::ids::{LocalGameId, OperationId, ProfileId, ProfileMemberId};
use onera_core::plan::InstallPlan;
use onera_core::ports::{JournalEntry, OperationJournal, ProfileStore, ReconciliationStore};
use onera_core::{CoreError, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A persistence call that can be made to fail.
///
/// Named after the port method rather than the SQL, because that is the
/// boundary a caller can reason about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DbCall {
    /// [`OperationJournal::begin`] or [`OperationJournal::begin_reconciliation`].
    Begin,
    /// [`OperationJournal::set_state`] — a journal state transition.
    SetState,
    /// [`OperationJournal::put_entry`] — a per-file journal record.
    PutEntry,
    /// [`ReconciliationStore::complete_reconciliation_publishing`] — the
    /// transaction that publishes a finished reconciliation.
    CompleteReconciliation,
    /// [`ProfileStore::set_active_profile`].
    SetActiveProfile,
    /// [`ProfileStore::record_activation`].
    RecordActivation,
    /// [`ProfileStore::put_member`].
    PutMember,
}

impl DbCall {
    fn message(self) -> &'static str {
        match self {
            Self::Begin => "injected failure opening the journal",
            Self::SetState => "injected failure recording a state transition",
            Self::PutEntry => "injected failure writing a journal entry",
            Self::CompleteReconciliation => "injected failure publishing a reconciliation",
            Self::SetActiveProfile => "injected failure activating a profile",
            Self::RecordActivation => "injected failure recording an activation",
            Self::PutMember => "injected failure writing a profile member",
        }
    }
}

/// Which persistence call to fail, and on which attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailAt {
    /// Never fail.
    Never,
    /// Fail the Nth call to this operation (0-indexed), letting earlier ones
    /// through. Failing the *second* transition is what catches a rollback that
    /// assumes it can still write.
    Nth(DbCall, usize),
    /// Fail the Nth call and every one after it.
    ///
    /// Models the database becoming unusable rather than one statement losing a
    /// race — a full disk, a deleted file, a corrupt page. It is the only way
    /// to reach a failure *inside* the rollback path, because rollback is only
    /// entered once something else has already failed.
    EveryAfter(DbCall, usize),
}

impl FailAt {
    /// Whether attempt `n` of `call` is one of the failures.
    fn selects(self, call: DbCall, n: usize) -> bool {
        match self {
            Self::Never => false,
            Self::Nth(selected, at) => selected == call && at == n,
            Self::EveryAfter(selected, from) => selected == call && n >= from,
        }
    }
}

/// Wraps a [`Database`] and injects one persistence failure.
///
/// Every call is delegated verbatim except the one selected. The wrapper counts
/// per call kind, so `Nth(SetState, 1)` means the second state transition
/// regardless of how many entries were written in between.
#[derive(Debug, Clone)]
pub struct FaultyDatabase {
    inner: Database,
    fail_at: FailAt,
    counts: Arc<Mutex<HashMap<DbCall, Arc<AtomicUsize>>>>,
}

impl FaultyDatabase {
    /// Wrap a database and fail at the given point.
    #[must_use]
    pub fn new(inner: Database, fail_at: FailAt) -> Self {
        Self {
            inner,
            fail_at,
            counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The database underneath, for assertions about what was really recorded.
    #[must_use]
    pub fn inner(&self) -> &Database {
        &self.inner
    }

    /// How many times a call has been attempted, injected failures included.
    #[must_use]
    pub fn attempts(&self, call: DbCall) -> usize {
        self.counter(call).load(Ordering::SeqCst)
    }

    fn counter(&self, call: DbCall) -> Arc<AtomicUsize> {
        Arc::clone(
            self.counts
                .lock()
                .expect("fault counter mutex is never held across a panic")
                .entry(call)
                .or_default(),
        )
    }

    /// Count an attempt and fail it if it is the selected one.
    fn check(&self, call: DbCall) -> Result<()> {
        let n = self.counter(call).fetch_add(1, Ordering::SeqCst);
        if self.fail_at.selects(call, n) {
            return Err(CoreError::Database(call.message().to_owned()));
        }
        Ok(())
    }
}

#[async_trait]
impl OperationJournal for FaultyDatabase {
    async fn begin(&self, plan: &InstallPlan, kind: OperationKind) -> Result<Operation> {
        self.check(DbCall::Begin)?;
        self.inner.begin(plan, kind).await
    }

    async fn begin_reconciliation(
        &self,
        plan: &MutationPlan,
        kind: OperationKind,
    ) -> Result<Operation> {
        self.check(DbCall::Begin)?;
        self.inner.begin_reconciliation(plan, kind).await
    }

    async fn set_state(
        &self,
        id: OperationId,
        state: OperationState,
        error: Option<&str>,
    ) -> Result<()> {
        self.check(DbCall::SetState)?;
        self.inner.set_state(id, state, error).await
    }

    async fn get(&self, id: OperationId) -> Result<Option<Operation>> {
        self.inner.get(id).await
    }

    async fn plan(&self, id: OperationId) -> Result<Option<InstallPlan>> {
        self.inner.plan(id).await
    }

    async fn put_entry(&self, id: OperationId, entry: &JournalEntry) -> Result<()> {
        self.check(DbCall::PutEntry)?;
        self.inner.put_entry(id, entry).await
    }

    async fn entries(&self, id: OperationId) -> Result<Vec<JournalEntry>> {
        self.inner.entries(id).await
    }

    async fn interrupted(&self) -> Result<Vec<Operation>> {
        self.inner.interrupted().await
    }
}

#[async_trait]
impl ReconciliationStore for FaultyDatabase {
    async fn complete_reconciliation_publishing(
        &self,
        operation: OperationId,
        plan: &MutationPlan,
        activate_profile: Option<ProfileId>,
    ) -> Result<()> {
        self.check(DbCall::CompleteReconciliation)?;
        self.inner
            .complete_reconciliation_publishing(operation, plan, activate_profile)
            .await
    }
}

#[async_trait]
impl ProfileStore for FaultyDatabase {
    async fn profiles(&self, game: LocalGameId) -> Result<Vec<Profile>> {
        self.inner.profiles(game).await
    }

    async fn profile(&self, id: ProfileId) -> Result<Option<Profile>> {
        self.inner.profile(id).await
    }

    async fn active_profile(&self, game: LocalGameId) -> Result<Option<Profile>> {
        self.inner.active_profile(game).await
    }

    async fn put_profile(&self, profile: &Profile) -> Result<()> {
        self.inner.put_profile(profile).await
    }

    async fn delete_profile(&self, id: ProfileId) -> Result<()> {
        self.inner.delete_profile(id).await
    }

    async fn set_active_profile(&self, game: LocalGameId, profile: ProfileId) -> Result<()> {
        self.check(DbCall::SetActiveProfile)?;
        self.inner.set_active_profile(game, profile).await
    }

    async fn members(&self, profile: ProfileId) -> Result<Vec<ProfileMember>> {
        self.inner.members(profile).await
    }

    async fn put_member(&self, member: &ProfileMember) -> Result<()> {
        self.check(DbCall::PutMember)?;
        self.inner.put_member(member).await
    }

    async fn remove_member(&self, member: ProfileMemberId) -> Result<()> {
        self.inner.remove_member(member).await
    }

    async fn record_activation(&self, activation: &ProfileActivation) -> Result<()> {
        self.check(DbCall::RecordActivation)?;
        self.inner.record_activation(activation).await
    }

    async fn activation_history(
        &self,
        game: LocalGameId,
        limit: u32,
    ) -> Result<Vec<ProfileActivation>> {
        self.inner.activation_history(game, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_never_failing_wrapper_is_transparent() {
        let db = Database::open_in_memory().await.unwrap();
        let faulty = FaultyDatabase::new(db, FailAt::Never);
        assert!(faulty.interrupted().await.unwrap().is_empty());
        assert_eq!(faulty.attempts(DbCall::SetState), 0);
    }

    #[tokio::test]
    async fn only_the_selected_attempt_fails() {
        let db = Database::open_in_memory().await.unwrap();
        let faulty = FaultyDatabase::new(db, FailAt::Nth(DbCall::SetState, 1));

        // The counter advances on every attempt, so the *second* call is the
        // one that fails and the third succeeds again.
        assert!(faulty.check(DbCall::SetState).is_ok());
        let err = faulty.check(DbCall::SetState).unwrap_err();
        assert!(matches!(err, CoreError::Database(_)), "{err:?}");
        assert!(faulty.check(DbCall::SetState).is_ok());
        assert_eq!(faulty.attempts(DbCall::SetState), 3);
    }

    #[tokio::test]
    async fn every_after_keeps_failing() {
        let db = Database::open_in_memory().await.unwrap();
        let faulty = FaultyDatabase::new(db, FailAt::EveryAfter(DbCall::SetState, 1));

        assert!(faulty.check(DbCall::SetState).is_ok());
        for _ in 0..3 {
            assert!(faulty.check(DbCall::SetState).is_err());
        }
        // Other calls are untouched: only this one operation went away.
        assert!(faulty.check(DbCall::PutEntry).is_ok());
    }

    #[tokio::test]
    async fn calls_are_counted_independently() {
        let db = Database::open_in_memory().await.unwrap();
        let faulty = FaultyDatabase::new(db, FailAt::Nth(DbCall::SetState, 0));

        // Entries written before the transition must not consume its budget.
        assert!(faulty.check(DbCall::PutEntry).is_ok());
        assert!(faulty.check(DbCall::PutEntry).is_ok());
        assert!(faulty.check(DbCall::SetState).is_err());
        assert_eq!(faulty.attempts(DbCall::PutEntry), 2);
    }
}
