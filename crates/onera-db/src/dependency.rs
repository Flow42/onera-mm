//! Persistence for provider dependency snapshots and accepted-risk overrides.
//!
//! The store records provider observations without making cache-freshness
//! decisions. Availability is stored as tagged JSON so every variant keeps its
//! data, while source identity remains in indexed columns for exact lookup.

use crate::convert::{from_timestamp, uuid};
use crate::{db_err, Database};
use async_trait::async_trait;
use chrono::SecondsFormat;
use onera_core::domain::dependency::{
    DependencyAvailability, DependencyFingerprint, DependencyGroup, DependencyOverride,
    DependencySnapshot, DependencySource, DlcRequirement,
};
use onera_core::ids::{
    DependencyGroupId, DependencySnapshotId, ProfileId, ProfileMemberId, ProviderFileId,
    ProviderId, ProviderModId, ProviderVersionId,
};
use onera_core::ports::DependencyStore;
use onera_core::{CoreError, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row as _, Sqlite};
use std::collections::{BTreeMap, BTreeSet};

const SNAPSHOT_COLUMNS: &str =
    "id, provider_id, game_slug, provider_mod_id, provider_file_id, provider_version_id, \
     availability_json, groups_json, dlc_json, provider_revision, fingerprint, fetched_at, raw_json";

fn encode_json<T: Serialize + ?Sized>(field: &str, value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| CoreError::Database(format!("cannot encode dependency {field}: {error}")))
}

fn decode_json<T: DeserializeOwned>(field: &str, value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|error| {
        CoreError::Database(format!("malformed stored dependency {field}: {error}"))
    })
}

fn decode_fingerprint(value: &str) -> Result<DependencyFingerprint> {
    decode_json(
        "fingerprint",
        &serde_json::Value::String(value.to_owned()).to_string(),
    )
}

fn snapshot_from_row(row: SqliteRow) -> Result<DependencySnapshot> {
    let id: String = row.try_get("id").map_err(db_err)?;
    let provider: String = row.try_get("provider_id").map_err(db_err)?;
    let provider_mod: String = row.try_get("provider_mod_id").map_err(db_err)?;
    let provider_file: Option<String> = row.try_get("provider_file_id").map_err(db_err)?;
    let provider_version: Option<String> = row.try_get("provider_version_id").map_err(db_err)?;
    let availability: String = row.try_get("availability_json").map_err(db_err)?;
    let groups: String = row.try_get("groups_json").map_err(db_err)?;
    let dlc: String = row.try_get("dlc_json").map_err(db_err)?;
    let fingerprint: String = row.try_get("fingerprint").map_err(db_err)?;
    let fetched_at: String = row.try_get("fetched_at").map_err(db_err)?;
    let raw: String = row.try_get("raw_json").map_err(db_err)?;

    Ok(DependencySnapshot {
        id: DependencySnapshotId::from(uuid(&id)?),
        source: DependencySource {
            provider: ProviderId::new(provider),
            game_slug: row.try_get("game_slug").map_err(db_err)?,
            provider_mod_id: ProviderModId::new(provider_mod),
            provider_file_id: provider_file.map(ProviderFileId::new),
            provider_version_id: provider_version.map(ProviderVersionId::new),
        },
        availability: decode_json::<DependencyAvailability>("availability", &availability)?,
        groups: decode_json::<Vec<DependencyGroup>>("groups", &groups)?,
        dlc: decode_json::<Vec<DlcRequirement>>("DLC requirements", &dlc)?,
        provider_revision: row.try_get("provider_revision").map_err(db_err)?,
        fingerprint: decode_fingerprint(&fingerprint)?,
        fetched_at: from_timestamp(&fetched_at)?,
        raw: decode_json("raw provider JSON", &raw)?,
    })
}

fn override_from_row(row: SqliteRow) -> Result<DependencyOverride> {
    let member: String = row.try_get("profile_member_id").map_err(db_err)?;
    let group: String = row.try_get("group_id").map_err(db_err)?;
    let fingerprint: String = row.try_get("fingerprint").map_err(db_err)?;
    let created_at: String = row.try_get("created_at").map_err(db_err)?;
    Ok(DependencyOverride {
        profile_member_id: ProfileMemberId::from(uuid(&member)?),
        fingerprint: decode_fingerprint(&fingerprint)?,
        group_id: DependencyGroupId::from(uuid(&group)?),
        reason: row.try_get("reason").map_err(db_err)?,
        created_at: from_timestamp(&created_at)?,
    })
}

impl Database {
    async fn snapshot_row(&self, source: &DependencySource) -> Result<Option<SqliteRow>> {
        sqlx::query(&format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM dependency_snapshots
             WHERE provider_id = ?1 AND game_slug = ?2 AND provider_mod_id = ?3
               AND provider_file_id IS ?4 AND provider_version_id IS ?5"
        ))
        .bind(source.provider.as_str())
        .bind(&source.game_slug)
        .bind(source.provider_mod_id.as_str())
        .bind(source.provider_file_id.as_ref().map(ProviderFileId::as_str))
        .bind(
            source
                .provider_version_id
                .as_ref()
                .map(ProviderVersionId::as_str),
        )
        .fetch_optional(self.pool())
        .await
        .map_err(db_err)
    }
}

#[async_trait]
impl DependencyStore for Database {
    async fn snapshot(&self, source: &DependencySource) -> Result<Option<DependencySnapshot>> {
        self.snapshot_row(source)
            .await?
            .map(snapshot_from_row)
            .transpose()
    }

    async fn snapshots(
        &self,
        sources: &[DependencySource],
    ) -> Result<Vec<Option<DependencySnapshot>>> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }

        let unique: Vec<_> = sources
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut stored = Vec::new();
        // Five bind parameters per source. Small chunks remain below even old
        // SQLite builds' host-parameter limit while retaining one ordered
        // result reconstruction for the whole caller request.
        for chunk in unique.chunks(100) {
            let mut query = QueryBuilder::<Sqlite>::new(format!(
                "SELECT {SNAPSHOT_COLUMNS} FROM dependency_snapshots WHERE "
            ));
            for (index, source) in chunk.iter().enumerate() {
                if index > 0 {
                    query.push(" OR ");
                }
                query
                    .push("(provider_id = ")
                    .push_bind(source.provider.to_string())
                    .push(" AND game_slug = ")
                    .push_bind(source.game_slug.clone())
                    .push(" AND provider_mod_id = ")
                    .push_bind(source.provider_mod_id.to_string())
                    .push(" AND provider_file_id IS ")
                    .push_bind(source.provider_file_id.as_ref().map(ToString::to_string))
                    .push(" AND provider_version_id IS ")
                    .push_bind(source.provider_version_id.as_ref().map(ToString::to_string))
                    .push(")");
            }
            stored.extend(query.build().fetch_all(self.pool()).await.map_err(db_err)?);
        }
        let snapshots: BTreeMap<_, _> = stored
            .into_iter()
            .map(snapshot_from_row)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|snapshot| (snapshot.source.clone(), snapshot))
            .collect();
        Ok(sources
            .iter()
            .map(|source| snapshots.get(source).cloned())
            .collect())
    }

    async fn put_snapshot(&self, snapshot: &DependencySnapshot) -> Result<()> {
        let availability = encode_json("availability", &snapshot.availability)?;
        let groups = encode_json("groups", &snapshot.groups)?;
        let dlc = encode_json("DLC requirements", &snapshot.dlc)?;
        let raw = encode_json("raw provider JSON", &snapshot.raw)?;
        let fetched_at = snapshot
            .fetched_at
            .to_rfc3339_opts(SecondsFormat::AutoSi, true);
        let mut tx = self.pool().begin().await.map_err(db_err)?;

        sqlx::query(
            "DELETE FROM dependency_snapshots
             WHERE provider_id = ?1 AND game_slug = ?2 AND provider_mod_id = ?3
               AND provider_file_id IS ?4 AND provider_version_id IS ?5",
        )
        .bind(snapshot.source.provider.as_str())
        .bind(&snapshot.source.game_slug)
        .bind(snapshot.source.provider_mod_id.as_str())
        .bind(
            snapshot
                .source
                .provider_file_id
                .as_ref()
                .map(ProviderFileId::as_str),
        )
        .bind(
            snapshot
                .source
                .provider_version_id
                .as_ref()
                .map(ProviderVersionId::as_str),
        )
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;

        sqlx::query(
            "INSERT INTO dependency_snapshots
                (id, provider_id, game_slug, provider_mod_id, provider_file_id,
                 provider_version_id, availability_json, groups_json, dlc_json,
                 provider_revision, fingerprint, fetched_at, raw_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(snapshot.id.to_string())
        .bind(snapshot.source.provider.as_str())
        .bind(&snapshot.source.game_slug)
        .bind(snapshot.source.provider_mod_id.as_str())
        .bind(
            snapshot
                .source
                .provider_file_id
                .as_ref()
                .map(ProviderFileId::as_str),
        )
        .bind(
            snapshot
                .source
                .provider_version_id
                .as_ref()
                .map(ProviderVersionId::as_str),
        )
        .bind(availability)
        .bind(groups)
        .bind(dlc)
        .bind(&snapshot.provider_revision)
        .bind(snapshot.fingerprint.as_str())
        .bind(fetched_at)
        .bind(raw)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)
    }

    async fn overrides(&self, profile: ProfileId) -> Result<Vec<DependencyOverride>> {
        let rows = sqlx::query(
            "SELECT o.profile_member_id, o.group_id, o.fingerprint, o.reason, o.created_at
             FROM dependency_overrides o
             JOIN profile_members m ON m.id = o.profile_member_id
             WHERE m.profile_id = ?1
             ORDER BY m.priority, o.profile_member_id, o.group_id",
        )
        .bind(profile.to_string())
        .fetch_all(self.pool())
        .await
        .map_err(db_err)?;
        rows.into_iter().map(override_from_row).collect()
    }

    async fn put_override(&self, decision: &DependencyOverride) -> Result<()> {
        sqlx::query(
            "INSERT INTO dependency_overrides
                (profile_member_id, group_id, fingerprint, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(profile_member_id, group_id) DO UPDATE SET
                fingerprint = ?3, reason = ?4, created_at = ?5",
        )
        .bind(decision.profile_member_id.to_string())
        .bind(decision.group_id.to_string())
        .bind(decision.fingerprint.as_str())
        .bind(&decision.reason)
        .bind(
            decision
                .created_at
                .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        )
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn delete_override(
        &self,
        member: ProfileMemberId,
        group: DependencyGroupId,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM dependency_overrides
             WHERE profile_member_id = ?1 AND group_id = ?2",
        )
        .bind(member.to_string())
        .bind(group.to_string())
        .execute(self.pool())
        .await
        .map_err(db_err)?;
        Ok(())
    }
}
