//! Profile and desired-member persistence.
//!
//! This module only changes SQLite desired state. It has no filesystem handle
//! and cannot publish deployment state; profile activation remains a separate
//! application flow.

use crate::convert::{from_timestamp, now, to_timestamp, uuid};
use crate::{db_err, Database};
use async_trait::async_trait;
use onera_core::domain::profile::{
    DesiredModState, MemberPin, MemberPriority, MemberSelection, Profile, ProfileActivation,
    ProfileActivationState, ProfileMember,
};
use onera_core::ids::{
    InstallationId, LocalGameId, ModId, ProfileId, ProfileMemberId, ProviderFileGroupId,
    ProviderFileId, ProviderId, ProviderModId, ProviderVersionId,
};
use onera_core::ports::ProfileStore;
use onera_core::{CoreError, Result};
use sqlx::sqlite::SqliteRow;
use sqlx::Row as _;

fn profile_from_row(row: SqliteRow) -> Result<Profile> {
    let id: String = row.try_get("id").map_err(db_err)?;
    let game: String = row.try_get("local_game_id").map_err(db_err)?;
    let active: i64 = row.try_get("is_active").map_err(db_err)?;
    let created: String = row.try_get("created_at").map_err(db_err)?;
    let updated: String = row.try_get("updated_at").map_err(db_err)?;
    Ok(Profile {
        id: ProfileId::from(uuid(&id)?),
        local_game_id: LocalGameId::from(uuid(&game)?),
        name: row.try_get("name").map_err(db_err)?,
        description: row.try_get("description").map_err(db_err)?,
        is_active: active != 0,
        created_at: from_timestamp(&created)?,
        updated_at: from_timestamp(&updated)?,
    })
}

fn member_from_row(row: SqliteRow) -> Result<ProfileMember> {
    let id: String = row.try_get("id").map_err(db_err)?;
    let profile: String = row.try_get("profile_id").map_err(db_err)?;
    let mod_id: String = row.try_get("mod_id").map_err(db_err)?;
    let provider: String = row.try_get("provider_id").map_err(db_err)?;
    let provider_mod: String = row.try_get("provider_mod_id").map_err(db_err)?;
    let provider_file: Option<String> = row.try_get("provider_file_id").map_err(db_err)?;
    let provider_version: Option<String> = row.try_get("provider_version_id").map_err(db_err)?;
    let provider_group: Option<String> = row.try_get("provider_file_group_id").map_err(db_err)?;
    let installation: Option<String> = row.try_get("installation_id").map_err(db_err)?;
    let desired: String = row.try_get("desired").map_err(db_err)?;
    let pinned: i64 = row.try_get("pinned").map_err(db_err)?;
    let pinned_at: Option<String> = row.try_get("pinned_at").map_err(db_err)?;
    let pin_reason: Option<String> = row.try_get("pin_reason").map_err(db_err)?;
    let priority: i64 = row.try_get("priority").map_err(db_err)?;
    let added: String = row.try_get("added_at").map_err(db_err)?;

    let desired = match desired.as_str() {
        "enabled" => DesiredModState::Enabled,
        "disabled" => DesiredModState::Disabled,
        other => {
            return Err(CoreError::Database(format!(
                "unknown desired state {other:?}"
            )))
        }
    };
    let pin = if pinned == 0 {
        MemberPin::Unpinned
    } else {
        MemberPin::Pinned {
            pinned_at: from_timestamp(pinned_at.as_deref().ok_or_else(|| {
                CoreError::Database("pinned profile member has no pinned_at".into())
            })?)?,
            reason: pin_reason,
        }
    };
    let priority = i32::try_from(priority)
        .map_err(|_| CoreError::Database("profile member priority is outside i32".into()))?;

    Ok(ProfileMember {
        id: ProfileMemberId::from(uuid(&id)?),
        profile_id: ProfileId::from(uuid(&profile)?),
        mod_id: ModId::from(uuid(&mod_id)?),
        selection: MemberSelection {
            provider: ProviderId::new(provider),
            provider_mod_id: ProviderModId::new(provider_mod),
            provider_file_id: provider_file.map(ProviderFileId::new),
            provider_version_id: provider_version.map(ProviderVersionId::new),
            provider_file_group_id: provider_group.map(ProviderFileGroupId::new),
        },
        installation_id: installation
            .as_deref()
            .map(uuid)
            .transpose()?
            .map(InstallationId::from),
        desired,
        pin,
        priority: MemberPriority(priority),
        added_at: from_timestamp(&added)?,
    })
}

fn desired_str(state: DesiredModState) -> &'static str {
    match state {
        DesiredModState::Enabled => "enabled",
        DesiredModState::Disabled => "disabled",
    }
}

fn activation_str(state: ProfileActivationState) -> &'static str {
    match state {
        ProfileActivationState::Preparing => "preparing",
        ProfileActivationState::Applying => "applying",
        ProfileActivationState::Applied => "applied",
        ProfileActivationState::RolledBack => "rolled_back",
        ProfileActivationState::Failed => "failed",
    }
}

fn parse_activation(value: &str) -> Result<ProfileActivationState> {
    Ok(match value {
        "preparing" => ProfileActivationState::Preparing,
        "applying" => ProfileActivationState::Applying,
        "applied" => ProfileActivationState::Applied,
        "rolled_back" => ProfileActivationState::RolledBack,
        "failed" => ProfileActivationState::Failed,
        other => {
            return Err(CoreError::Database(format!(
                "unknown profile activation state {other:?}"
            )))
        }
    })
}

fn profile_conflict(error: sqlx::Error, context: &str) -> CoreError {
    let detail = error.to_string();
    if detail.contains("UNIQUE constraint failed")
        || detail.contains("profile member installation belongs")
    {
        CoreError::Conflict(format!("{context}: {detail}"))
    } else {
        db_err(error)
    }
}

impl Database {
    /// Fetch one member by its membership identity.
    pub async fn profile_member(&self, id: ProfileMemberId) -> Result<Option<ProfileMember>> {
        sqlx::query("SELECT * FROM profile_members WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(db_err)?
            .map(member_from_row)
            .transpose()
    }

    /// Activation attempts that never reached a terminal state.
    ///
    /// Read on startup: every row here belongs to a process that died mid
    /// switch. None of them may be reported as `applied` — the target profile
    /// is active only when the completion transaction said so.
    pub async fn interrupted_activations(&self) -> Result<Vec<ProfileActivation>> {
        let rows = sqlx::query(
            "SELECT from_profile_id, to_profile_id, operation_id, state,
                    started_at, finished_at, error
             FROM profile_activation_history
             WHERE state IN ('preparing', 'applying')
             ORDER BY started_at, id",
        )
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(activation_from_row).collect()
    }

    /// Resolve a mod/file choice into the opaque selection and any retained
    /// installation that already satisfies it for this profile's game.
    pub async fn selection_for_profile_member(
        &self,
        profile: ProfileId,
        mod_id: ModId,
        requested_file: Option<&ProviderFileId>,
    ) -> Result<(MemberSelection, Option<InstallationId>)> {
        let scope = sqlx::query(
            "SELECT p.local_game_id, m.provider_id, m.provider_mod_id
             FROM profiles p JOIN mods m ON m.id = ?2 WHERE p.id = ?1",
        )
        .bind(profile.to_string())
        .bind(mod_id.to_string())
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?
        .ok_or_else(|| CoreError::NotFound {
            kind: "profile or mod",
            id: format!("{profile}/{mod_id}"),
        })?;
        let game: String = scope.try_get("local_game_id").map_err(db_err)?;
        let provider: String = scope.try_get("provider_id").map_err(db_err)?;
        let provider_mod: String = scope.try_get("provider_mod_id").map_err(db_err)?;

        let row = if let Some(file) = requested_file {
            sqlx::query(
                "SELECT pf.id AS stored_file_id, pf.provider_file_id,
                        pf.provider_version_id, pf.provider_file_group_id,
                        i.id AS installation_id
                 FROM provider_files pf
                 JOIN releases r ON r.id = pf.release_id
                 LEFT JOIN installations i ON i.id = (
                     SELECT candidate.id FROM installations candidate
                     JOIN archive_provider_files apf
                       ON apf.archive_id = candidate.archive_id
                     WHERE candidate.local_game_id = ?1
                       AND candidate.mod_id = ?2
                       AND candidate.release_id = pf.release_id
                       AND apf.provider_file_id = pf.id
                     ORDER BY candidate.active DESC,
                              candidate.installed_at DESC, candidate.id DESC LIMIT 1
                 )
                 WHERE pf.provider_id = ?3 AND pf.provider_file_id = ?4
                   AND r.mod_id = ?2",
            )
            .bind(&game)
            .bind(mod_id.to_string())
            .bind(&provider)
            .bind(file.as_str())
            .fetch_optional(self.pool())
            .await
            .map_err(db_err)?
            .ok_or_else(|| CoreError::NotFound {
                kind: "provider file for mod",
                id: file.to_string(),
            })?
        } else {
            let installed = sqlx::query(
                "SELECT i.id AS installation_id, pf.provider_file_id,
                        pf.provider_version_id, pf.provider_file_group_id
                 FROM installations i
                 LEFT JOIN provider_files pf ON pf.id = (
                     SELECT linked.id FROM archive_provider_files apf
                     JOIN provider_files linked ON linked.id = apf.provider_file_id
                     WHERE apf.archive_id = i.archive_id
                       AND linked.release_id = i.release_id
                       AND linked.provider_id = ?3
                     ORDER BY linked.provider_file_id, linked.id LIMIT 1
                 )
                 WHERE i.local_game_id = ?1 AND i.mod_id = ?2
                 ORDER BY i.active DESC, i.installed_at DESC, i.id DESC LIMIT 1",
            )
            .bind(&game)
            .bind(mod_id.to_string())
            .bind(&provider)
            .fetch_optional(self.pool())
            .await
            .map_err(db_err)?;
            match installed {
                Some(row) => row,
                None => {
                    return Ok((
                        MemberSelection::unresolved(
                            ProviderId::new(provider),
                            ProviderModId::new(provider_mod),
                        ),
                        None,
                    ))
                }
            }
        };

        let installation: Option<String> = row.try_get("installation_id").map_err(db_err)?;
        Ok((
            MemberSelection {
                provider: ProviderId::new(provider),
                provider_mod_id: ProviderModId::new(provider_mod),
                provider_file_id: row
                    .try_get::<Option<String>, _>("provider_file_id")
                    .map_err(db_err)?
                    .map(ProviderFileId::new),
                provider_version_id: row
                    .try_get::<Option<String>, _>("provider_version_id")
                    .map_err(db_err)?
                    .map(ProviderVersionId::new),
                provider_file_group_id: row
                    .try_get::<Option<String>, _>("provider_file_group_id")
                    .map_err(db_err)?
                    .map(ProviderFileGroupId::new),
            },
            installation
                .as_deref()
                .map(uuid)
                .transpose()?
                .map(InstallationId::from),
        ))
    }
}

#[async_trait]
impl ProfileStore for Database {
    async fn profiles(&self, game: LocalGameId) -> Result<Vec<Profile>> {
        let rows = sqlx::query(
            "SELECT * FROM profiles WHERE local_game_id = ?1
             ORDER BY is_active DESC, name COLLATE NOCASE, id",
        )
        .bind(game.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(profile_from_row).collect()
    }

    async fn profile(&self, id: ProfileId) -> Result<Option<Profile>> {
        sqlx::query("SELECT * FROM profiles WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(db_err)?
            .map(profile_from_row)
            .transpose()
    }

    async fn active_profile(&self, game: LocalGameId) -> Result<Option<Profile>> {
        sqlx::query("SELECT * FROM profiles WHERE local_game_id = ?1 AND is_active = 1")
            .bind(game.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(db_err)?
            .map(profile_from_row)
            .transpose()
    }

    async fn put_profile(&self, profile: &Profile) -> Result<()> {
        if profile.name.trim().is_empty() {
            return Err(CoreError::InvalidInput(
                "profile name cannot be empty".into(),
            ));
        }
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let game_exists: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM local_game_installs WHERE id = ?1 AND confirmed = 1")
                .bind(profile.local_game_id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if game_exists.is_none() {
            return Err(CoreError::NotFound {
                kind: "registered local game",
                id: profile.local_game_id.to_string(),
            });
        }
        let existing: Option<(String, i64)> =
            sqlx::query_as("SELECT local_game_id, is_active FROM profiles WHERE id = ?1")
                .bind(profile.id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if let Some((game, active)) = &existing {
            if game != &profile.local_game_id.to_string() {
                return Err(CoreError::Conflict(
                    "a profile cannot move to another game".into(),
                ));
            }
            if (*active != 0) != profile.is_active {
                return Err(CoreError::Conflict(
                    "use atomic active-profile selection to change the active profile".into(),
                ));
            }
        } else if profile.is_active {
            let count: (i64,) =
                sqlx::query_as("SELECT count(*) FROM profiles WHERE local_game_id = ?1")
                    .bind(profile.local_game_id.to_string())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?;
            if count.0 != 0 {
                return Err(CoreError::Conflict(
                    "use atomic active-profile selection to change the active profile".into(),
                ));
            }
        }
        sqlx::query(
            "INSERT INTO profiles
                (id, local_game_id, name, description, is_active, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET name = ?3, description = ?4, updated_at = ?7",
        )
        .bind(profile.id.to_string())
        .bind(profile.local_game_id.to_string())
        .bind(&profile.name)
        .bind(&profile.description)
        .bind(i64::from(profile.is_active))
        .bind(to_timestamp(profile.created_at))
        .bind(to_timestamp(profile.updated_at))
        .execute(&mut *tx)
        .await
        .map_err(|error| profile_conflict(error, "profile name is already in use"))?;
        tx.commit().await.map_err(db_err)
    }

    async fn delete_profile(&self, id: ProfileId) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let active: Option<(i64,)> = sqlx::query_as("SELECT is_active FROM profiles WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
        match active {
            None => {
                return Err(CoreError::NotFound {
                    kind: "profile",
                    id: id.to_string(),
                })
            }
            Some((1,)) => {
                return Err(CoreError::Conflict(
                    "the active profile cannot be deleted".into(),
                ))
            }
            Some(_) => {}
        }
        sqlx::query("DELETE FROM profiles WHERE id = ?1")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)
    }

    async fn set_active_profile(&self, game: LocalGameId, profile: ProfileId) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let owner: Option<(String,)> =
            sqlx::query_as("SELECT local_game_id FROM profiles WHERE id = ?1")
                .bind(profile.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        match owner {
            None => {
                return Err(CoreError::NotFound {
                    kind: "profile",
                    id: profile.to_string(),
                })
            }
            Some((owner,)) if owner != game.to_string() => {
                return Err(CoreError::Conflict(format!(
                    "profile {profile} belongs to another game"
                )))
            }
            Some(_) => {}
        }
        let changed_at = now();
        sqlx::query("UPDATE profiles SET is_active = 0, updated_at = ?2 WHERE local_game_id = ?1 AND is_active = 1")
            .bind(game.to_string())
            .bind(&changed_at)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("UPDATE profiles SET is_active = 1, updated_at = ?2 WHERE id = ?1")
            .bind(profile.to_string())
            .bind(changed_at)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)
    }

    async fn members(&self, profile: ProfileId) -> Result<Vec<ProfileMember>> {
        let rows = sqlx::query(
            "SELECT * FROM profile_members WHERE profile_id = ?1 ORDER BY priority, id",
        )
        .bind(profile.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(member_from_row).collect()
    }

    async fn put_member(&self, member: &ProfileMember) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let scope = sqlx::query(
            "SELECT p.local_game_id, m.provider_id, m.provider_mod_id
             FROM profiles p JOIN mods m ON m.id = ?2 WHERE p.id = ?1",
        )
        .bind(member.profile_id.to_string())
        .bind(member.mod_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?
        .ok_or_else(|| CoreError::NotFound {
            kind: "profile or mod",
            id: format!("{}/{}", member.profile_id, member.mod_id),
        })?;
        let game: String = scope.try_get("local_game_id").map_err(db_err)?;
        let provider: String = scope.try_get("provider_id").map_err(db_err)?;
        let provider_mod: String = scope.try_get("provider_mod_id").map_err(db_err)?;
        if provider != member.selection.provider.as_str()
            || provider_mod != member.selection.provider_mod_id.as_str()
        {
            return Err(CoreError::Conflict(
                "member selection does not belong to its mod lineage".into(),
            ));
        }
        if let Some(installation) = member.installation_id {
            let valid: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM installations
                 WHERE id = ?1 AND local_game_id = ?2 AND mod_id = ?3",
            )
            .bind(installation.to_string())
            .bind(&game)
            .bind(member.mod_id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            if valid.is_none() {
                return Err(CoreError::Conflict(
                    "a profile cannot reference another game's or mod's installation".into(),
                ));
            }
        }
        let existing: Option<(String, String)> =
            sqlx::query_as("SELECT profile_id, mod_id FROM profile_members WHERE id = ?1")
                .bind(member.id.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if existing.as_ref().is_some_and(|value| {
            value != &(member.profile_id.to_string(), member.mod_id.to_string())
        }) {
            return Err(CoreError::Conflict(
                "a profile membership cannot move to another profile or mod".into(),
            ));
        }
        let (pinned, pinned_at, reason) = match &member.pin {
            MemberPin::Unpinned => (0_i64, None, None),
            MemberPin::Pinned { pinned_at, reason } => {
                (1, Some(to_timestamp(*pinned_at)), reason.clone())
            }
        };
        sqlx::query(
            "INSERT INTO profile_members
                (id, profile_id, mod_id, provider_id, provider_mod_id,
                 provider_file_id, provider_version_id, provider_file_group_id,
                 installation_id, desired, pinned, pinned_at, pin_reason, priority, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET
                 provider_file_id = ?6, provider_version_id = ?7,
                 provider_file_group_id = ?8, installation_id = ?9,
                 desired = ?10, pinned = ?11, pinned_at = ?12,
                 pin_reason = ?13, priority = ?14",
        )
        .bind(member.id.to_string())
        .bind(member.profile_id.to_string())
        .bind(member.mod_id.to_string())
        .bind(member.selection.provider.as_str())
        .bind(member.selection.provider_mod_id.as_str())
        .bind(
            member
                .selection
                .provider_file_id
                .as_ref()
                .map(ProviderFileId::as_str),
        )
        .bind(
            member
                .selection
                .provider_version_id
                .as_ref()
                .map(ProviderVersionId::as_str),
        )
        .bind(
            member
                .selection
                .provider_file_group_id
                .as_ref()
                .map(ProviderFileGroupId::as_str),
        )
        .bind(member.installation_id.map(|id| id.to_string()))
        .bind(desired_str(member.desired))
        .bind(pinned)
        .bind(pinned_at)
        .bind(reason)
        .bind(i64::from(member.priority.0))
        .bind(to_timestamp(member.added_at))
        .execute(&mut *tx)
        .await
        .map_err(|error| profile_conflict(error, "mod is already a member of this profile"))?;
        sqlx::query("UPDATE profiles SET updated_at = ?2 WHERE id = ?1")
            .bind(member.profile_id.to_string())
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)
    }

    async fn remove_member(&self, member: ProfileMemberId) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(db_err)?;
        let profile: Option<(String,)> =
            sqlx::query_as("SELECT profile_id FROM profile_members WHERE id = ?1")
                .bind(member.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        let Some((profile,)) = profile else {
            return Err(CoreError::NotFound {
                kind: "profile member",
                id: member.to_string(),
            });
        };
        sqlx::query("DELETE FROM profile_members WHERE id = ?1")
            .bind(member.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("UPDATE profiles SET updated_at = ?2 WHERE id = ?1")
            .bind(profile)
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)
    }

    async fn record_activation(&self, activation: &ProfileActivation) -> Result<()> {
        let id: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM profile_activation_history
             WHERE to_profile_id = ?1 AND started_at = ?2",
        )
        .bind(activation.to_profile_id.to_string())
        .bind(to_timestamp(activation.started_at))
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO profile_activation_history
                (id, from_profile_id, to_profile_id, operation_id, state,
                 started_at, finished_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(to_profile_id, started_at) DO UPDATE SET
                 from_profile_id = ?2, operation_id = ?4, state = ?5,
                 finished_at = ?7, error = ?8",
        )
        .bind(id.map_or_else(|| uuid::Uuid::new_v4().to_string(), |(id,)| id))
        .bind(activation.from_profile_id.map(|id| id.to_string()))
        .bind(activation.to_profile_id.to_string())
        .bind(activation.operation_id.map(|id| id.to_string()))
        .bind(activation_str(activation.state))
        .bind(to_timestamp(activation.started_at))
        .bind(activation.finished_at.map(to_timestamp))
        .bind(&activation.error)
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn activation_history(
        &self,
        game: LocalGameId,
        limit: u32,
    ) -> Result<Vec<ProfileActivation>> {
        let rows = sqlx::query(
            "SELECT h.from_profile_id, h.to_profile_id, h.operation_id,
                    h.state, h.started_at, h.finished_at, h.error
             FROM profile_activation_history h
             JOIN profiles p ON p.id = h.to_profile_id
             WHERE p.local_game_id = ?1
             ORDER BY h.started_at DESC, h.id DESC LIMIT ?2",
        )
        .bind(game.to_string())
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(activation_from_row).collect()
    }
}

fn activation_from_row(row: SqliteRow) -> Result<ProfileActivation> {
    let from: Option<String> = row.try_get("from_profile_id").map_err(db_err)?;
    let to: String = row.try_get("to_profile_id").map_err(db_err)?;
    let operation: Option<String> = row.try_get("operation_id").map_err(db_err)?;
    let state: String = row.try_get("state").map_err(db_err)?;
    let started: String = row.try_get("started_at").map_err(db_err)?;
    let finished: Option<String> = row.try_get("finished_at").map_err(db_err)?;
    Ok(ProfileActivation {
        from_profile_id: from.as_deref().map(uuid).transpose()?.map(ProfileId::from),
        to_profile_id: ProfileId::from(uuid(&to)?),
        operation_id: operation
            .as_deref()
            .map(uuid)
            .transpose()?
            .map(onera_core::ids::OperationId::from),
        state: parse_activation(&state)?,
        started_at: from_timestamp(&started)?,
        finished_at: finished.as_deref().map(from_timestamp).transpose()?,
        error: row.try_get("error").map_err(db_err)?,
    })
}
