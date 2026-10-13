//! Media's own rows: one per `run` Task, in the `media` schema. A row records the run's
//! owner, model, provider prediction, status and stored outputs for the resources Media
//! serves; core's Task runtime holds the Task, its steps and its provider operations.
//! Reads filter by owner and labels inside each statement, so a run the reader may not
//! see reads as missing.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use uuid::Uuid;
use veoveo_platform_store::{
    CursorScope, KeysetCursor, Page, PageSize, PlatformStore, PrincipalId as StoredPrincipal,
    TenantId as StoredTenant,
};
use veoveo_types::{DataLabelId, PrincipalId, TaskId, TenantId};

use crate::contract::{
    GeneratedArtifact, ListCursor, MediaModelId, MediaPredictionId, PredictionStatus,
    PredictionSummary,
};

/// The most runs one page holds.
pub const MAX_PREDICTIONS_PER_PAGE: u32 = 100;
/// A page size of the prediction listing.
pub type PredictionPageSize = PageSize<MAX_PREDICTIONS_PER_PAGE>;
/// The most runs one sweep batch deletes.
pub const PRUNE_BATCH: i64 = 500;

/// Why a store call failed.
#[derive(Debug, thiserror::Error)]
pub enum MediaStoreError {
    #[error("the run names no stored tenant or principal")]
    InvalidOwner,
    #[error("the cursor belongs to another listing; start from the first page")]
    StaleCursor,
    #[error("a stored run does not decode: {0}")]
    Corrupt(&'static str),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Who a run belongs to, or who reads runs: tenant, principal and data labels. A reader
/// sees the runs it owns whose labels it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOwner {
    tenant: StoredTenant,
    principal: StoredPrincipal,
    labels: BTreeSet<DataLabelId>,
}

impl RunOwner {
    pub fn new(
        tenant: &TenantId,
        principal: &PrincipalId,
        labels: BTreeSet<DataLabelId>,
    ) -> Result<Self, MediaStoreError> {
        let stored_tenant = veoveo_platform_store::deterministic_tenant_id(tenant.as_str())
            .map_err(|_| MediaStoreError::InvalidOwner)?;
        let stored_principal =
            veoveo_platform_store::deterministic_principal_id(tenant.as_str(), principal.as_str())
                .map_err(|_| MediaStoreError::InvalidOwner)?;
        Ok(Self {
            tenant: stored_tenant,
            principal: stored_principal,
            labels,
        })
    }

    pub fn tenant(&self) -> StoredTenant {
        self.tenant
    }

    pub fn labels(&self) -> &BTreeSet<DataLabelId> {
        &self.labels
    }

    fn label_keys(&self) -> Vec<String> {
        self.labels.iter().map(ToString::to_string).collect()
    }
}

/// One run as the database holds it.
#[derive(Debug, Clone)]
pub struct PredictionRow {
    pub tenant: StoredTenant,
    pub task_id: Uuid,
    pub owner: StoredPrincipal,
    pub data_labels: Vec<String>,
    pub model_id: String,
    pub prediction_id: Option<String>,
    pub status_url: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub outputs: Json<Vec<GeneratedArtifact>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl PredictionRow {
    pub fn task_id(&self) -> TaskId {
        TaskId::from_uuid(self.task_id)
    }

    pub fn prediction_id(&self) -> Result<Option<MediaPredictionId>, MediaStoreError> {
        self.prediction_id
            .as_deref()
            .map(|id| {
                MediaPredictionId::parse(id).map_err(|_| MediaStoreError::Corrupt("prediction"))
            })
            .transpose()
    }

    pub fn status(&self) -> Result<PredictionStatus, MediaStoreError> {
        self.status
            .parse()
            .map_err(|_| MediaStoreError::Corrupt("status"))
    }

    pub fn outputs(&self) -> &[GeneratedArtifact] {
        &self.outputs.0
    }

    pub fn summary(&self) -> Result<PredictionSummary, MediaStoreError> {
        Ok(PredictionSummary {
            task_id: self.task_id(),
            prediction_id: self.prediction_id()?,
            model_id: MediaModelId::parse(self.model_id.as_str())
                .map_err(|_| MediaStoreError::Corrupt("model"))?,
            status: self.status()?,
            error: self.error.clone(),
            output_count: u32::try_from(self.outputs.0.len())
                .map_err(|_| MediaStoreError::Corrupt("outputs"))?,
            created_at: self.created_at,
            completed_at: self.completed_at,
        })
    }
}

/// A page of runs and the cursor of the next one.
pub type PredictionPage = Page<PredictionRow, ListCursor>;

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnding {
    Succeeded(Vec<GeneratedArtifact>),
    Failed(String),
    /// Cancelled, with any outputs stored before the cancellation arrived.
    Cancelled(Vec<GeneratedArtifact>),
    Unresolved,
}

impl RunEnding {
    fn status(&self) -> PredictionStatus {
        match self {
            Self::Succeeded(_) => PredictionStatus::Succeeded,
            Self::Failed(_) => PredictionStatus::Failed,
            Self::Cancelled(_) => PredictionStatus::Cancelled,
            Self::Unresolved => PredictionStatus::Unresolved,
        }
    }
}

/// The longest error a run keeps, in bytes.
const MAX_ERROR_BYTES: usize = 2048;

fn bounded_error(text: &str) -> Option<String> {
    let mut end = text.len().min(MAX_ERROR_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let bounded = text[..end].trim();
    (!bounded.is_empty()).then(|| bounded.to_owned())
}

/// Media's store over its server login.
#[derive(Clone)]
pub struct MediaStore {
    platform: PlatformStore,
}

impl MediaStore {
    pub fn new(platform: PlatformStore) -> Self {
        Self { platform }
    }

    pub fn platform(&self) -> &PlatformStore {
        &self.platform
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.platform.pool()
    }

    /// Record the owner's run of `model` as its Task starts. A repeat changes nothing.
    pub async fn record_run(
        &self,
        owner: &RunOwner,
        task_id: TaskId,
        model: &MediaModelId,
        now: DateTime<Utc>,
    ) -> Result<(), MediaStoreError> {
        sqlx::query_file!(
            "src/store/predictions/record_run.sql",
            owner.tenant as StoredTenant,
            task_id.as_uuid(),
            owner.principal as StoredPrincipal,
            &owner.label_keys(),
            model.as_str(),
            now,
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The provider accepted the run, under its prediction and status endpoint when it
    /// named them.
    pub async fn mark_processing(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
        prediction: Option<&MediaPredictionId>,
        status_url: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), MediaStoreError> {
        sqlx::query_file!(
            "src/store/predictions/mark_processing.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
            prediction.map(MediaPredictionId::as_str),
            status_url,
            now,
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The provider finished the run, and Media stores its outputs.
    pub async fn mark_storing(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
        prediction: &MediaPredictionId,
        now: DateTime<Utc>,
    ) -> Result<(), MediaStoreError> {
        sqlx::query_file!(
            "src/store/predictions/mark_storing.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
            prediction.as_str(),
            now,
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// End the run. An ended run keeps its first ending.
    pub async fn finish(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
        prediction: Option<&MediaPredictionId>,
        ending: &RunEnding,
        now: DateTime<Utc>,
    ) -> Result<(), MediaStoreError> {
        let (error, outputs) = match ending {
            RunEnding::Succeeded(outputs) | RunEnding::Cancelled(outputs) => {
                (None, outputs.clone())
            }
            RunEnding::Failed(error) => (bounded_error(error), Vec::new()),
            RunEnding::Unresolved => (None, Vec::new()),
        };
        sqlx::query_file!(
            "src/store/predictions/finish.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
            ending.status().as_str(),
            error,
            Json(outputs) as Json<Vec<GeneratedArtifact>>,
            prediction.map(MediaPredictionId::as_str),
            now,
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// One run by its Task, for the worker that runs it.
    pub async fn run(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
    ) -> Result<Option<PredictionRow>, MediaStoreError> {
        Ok(sqlx::query_file_as!(
            PredictionRow,
            "src/store/predictions/by_task.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
        )
        .fetch_optional(self.pool())
        .await?)
    }

    /// The status endpoint the provider named for a run's prediction, by its Task.
    pub async fn status_url(&self, task_id: TaskId) -> Result<Option<String>, MediaStoreError> {
        Ok(
            sqlx::query_file_scalar!("src/store/predictions/status_url.sql", task_id.as_uuid())
                .fetch_optional(self.pool())
                .await?
                .flatten(),
        )
    }

    /// One prediction the reader may see, by the provider's identity.
    pub async fn prediction(
        &self,
        reader: &RunOwner,
        prediction: &MediaPredictionId,
    ) -> Result<Option<PredictionRow>, MediaStoreError> {
        Ok(sqlx::query_file_as!(
            PredictionRow,
            "src/store/predictions/visible.sql",
            reader.tenant as StoredTenant,
            prediction.as_str(),
            reader.principal as StoredPrincipal,
            &reader.label_keys(),
        )
        .fetch_optional(self.pool())
        .await?)
    }

    /// One page of the reader's runs, newest first.
    pub async fn predictions_page(
        &self,
        reader: &RunOwner,
        cursor: Option<&ListCursor>,
        size: PredictionPageSize,
    ) -> Result<PredictionPage, MediaStoreError> {
        let scope = CursorScope::new("media.predictions", reader.tenant, &reader.principal)
            .map_err(|_| MediaStoreError::StaleCursor)?;
        let after: Option<KeysetCursor<Uuid>> = cursor
            .map(|wire| scope.parse(wire.as_str()))
            .transpose()
            .map_err(|_| MediaStoreError::StaleCursor)?;
        let labels = reader.label_keys();
        let limit = size.fetch_limit();
        let rows = match after {
            None => {
                sqlx::query_file_as!(
                    PredictionRow,
                    "src/store/predictions/page_first.sql",
                    reader.tenant as StoredTenant,
                    reader.principal as StoredPrincipal,
                    &labels,
                    limit,
                )
                .fetch_all(self.pool())
                .await?
            }
            Some(after) => {
                sqlx::query_file_as!(
                    PredictionRow,
                    "src/store/predictions/page_next.sql",
                    reader.tenant as StoredTenant,
                    reader.principal as StoredPrincipal,
                    &labels,
                    *after.position(),
                    limit,
                )
                .fetch_all(self.pool())
                .await?
            }
        };
        size.page(rows, |last| last.task_id)
            .try_map_next(|position| {
                let cursor = scope
                    .cursor(position)
                    .map_err(|_| MediaStoreError::StaleCursor)?;
                ListCursor::parse(cursor.as_str()).map_err(|_| MediaStoreError::StaleCursor)
            })
    }

    /// Delete one batch of runs that ended before `cutoff`, across tenants.
    pub async fn prune(&self, cutoff: DateTime<Utc>) -> Result<u64, MediaStoreError> {
        let deleted = sqlx::query_file!("src/store/predictions/prune.sql", cutoff, PRUNE_BATCH)
            .execute(self.pool())
            .await?;
        Ok(deleted.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_bounded_at_a_character_boundary() {
        assert_eq!(bounded_error("  "), None);
        assert_eq!(bounded_error(" boom "), Some("boom".into()));
        let long = "é".repeat(MAX_ERROR_BYTES);
        let bounded = bounded_error(&long).unwrap();
        assert!(bounded.len() <= MAX_ERROR_BYTES);
        assert!(bounded.chars().all(|c| c == 'é'));
    }
}
