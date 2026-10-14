//! Media's own rows: one per `run` Task, in the `media` schema. A row holds what only
//! Media knows about the run: its model, the provider's prediction and status endpoint,
//! how the provider's outcome reaches it, its stored outputs and when Media recorded it.
//! Core's Task runtime holds the rest, the run's owner, status, failure and times, and
//! Media reads them through the runtime's API, so no row can disagree with its Task.
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use uuid::Uuid;
use veoveo_platform_store::{PlatformStore, TenantId as StoredTenant};
use veoveo_types::{TaskId, TenantId};

use crate::contract::{CompletionMode, GeneratedArtifact, MediaModelId, MediaPredictionId};

/// The most runs one sweep batch reads.
pub const SWEEP_BATCH: i64 = 500;

/// The unique index under which a provider names each prediction once per tenant.
const PREDICTION_INDEX: &str = "prediction_provider";

/// Why a store call failed.
#[derive(Debug, thiserror::Error)]
pub enum MediaStoreError {
    #[error("the run names no stored tenant")]
    InvalidTenant,
    #[error("a stored run does not decode: {0}")]
    Corrupt(&'static str),
    #[error("the provider named prediction {0}, which another run already holds")]
    PredictionConflict(MediaPredictionId),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl MediaStoreError {
    /// A write's failure, naming the prediction when the provider's identity of it
    /// collides with another run's.
    fn of_write(error: sqlx::Error, prediction: Option<&MediaPredictionId>) -> Self {
        match (&error, prediction) {
            (sqlx::Error::Database(database), Some(prediction))
                if database.constraint() == Some(PREDICTION_INDEX) =>
            {
                Self::PredictionConflict(prediction.clone())
            }
            _ => Self::Database(error),
        }
    }
}

/// The stored identity of a tenant, as the identity schema derives it.
pub fn stored_tenant(tenant: &TenantId) -> Result<StoredTenant, MediaStoreError> {
    veoveo_platform_store::deterministic_tenant_id(tenant.as_str())
        .map_err(|_| MediaStoreError::InvalidTenant)
}

/// One run as Media holds it.
#[derive(Debug, Clone)]
pub struct PredictionRow {
    pub tenant: StoredTenant,
    pub task_id: Uuid,
    pub model_id: String,
    pub prediction_id: Option<String>,
    pub status_url: Option<String>,
    pub completion: Option<String>,
    pub outputs: Json<Vec<GeneratedArtifact>>,
    pub created_at: DateTime<Utc>,
}

impl PredictionRow {
    pub fn task_id(&self) -> TaskId {
        TaskId::from_uuid(self.task_id)
    }

    pub fn model_id(&self) -> Result<MediaModelId, MediaStoreError> {
        MediaModelId::parse(self.model_id.as_str()).map_err(|_| MediaStoreError::Corrupt("model"))
    }

    pub fn prediction_id(&self) -> Result<Option<MediaPredictionId>, MediaStoreError> {
        self.prediction_id
            .as_deref()
            .map(|id| {
                MediaPredictionId::parse(id).map_err(|_| MediaStoreError::Corrupt("prediction"))
            })
            .transpose()
    }

    pub fn completion(&self) -> Result<Option<CompletionMode>, MediaStoreError> {
        self.completion
            .as_deref()
            .map(|mode| {
                mode.parse()
                    .map_err(|_| MediaStoreError::Corrupt("completion"))
            })
            .transpose()
    }

    pub fn outputs(&self) -> &[GeneratedArtifact] {
        &self.outputs.0
    }
}

/// A run past its retention, as the sweep walks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgedRun {
    pub tenant: StoredTenant,
    pub task_id: TaskId,
    pub created_at: DateTime<Utc>,
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

    /// Record a run of `model` as its Task starts. A repeat changes nothing.
    pub async fn record_run(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
        model: &MediaModelId,
        now: DateTime<Utc>,
    ) -> Result<(), MediaStoreError> {
        sqlx::query_file!(
            "src/store/predictions/record_run.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
            model.as_str(),
            now,
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The provider accepted the run, under its prediction and status endpoint when it
    /// named them, and with how its outcome reaches the run once that is known.
    pub async fn record_submission(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
        prediction: Option<&MediaPredictionId>,
        status_url: Option<&str>,
        completion: Option<CompletionMode>,
    ) -> Result<(), MediaStoreError> {
        sqlx::query_file!(
            "src/store/predictions/record_submission.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
            prediction.map(MediaPredictionId::as_str),
            status_url,
            completion.as_ref().map(CompletionMode::as_str),
        )
        .execute(self.pool())
        .await
        .map_err(|error| MediaStoreError::of_write(error, prediction))?;
        Ok(())
    }

    /// The run's outputs, stored in Artifact, under the prediction that made them.
    pub async fn record_outputs(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
        prediction: &MediaPredictionId,
        outputs: &[GeneratedArtifact],
    ) -> Result<(), MediaStoreError> {
        sqlx::query_file!(
            "src/store/predictions/record_outputs.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
            Json(outputs.to_vec()) as Json<Vec<GeneratedArtifact>>,
            prediction.as_str(),
        )
        .execute(self.pool())
        .await
        .map_err(|error| MediaStoreError::of_write(error, Some(prediction)))?;
        Ok(())
    }

    /// One run by its Task.
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

    /// One run by the provider's prediction. Whether a caller may see it is its Task's
    /// to decide.
    pub async fn by_prediction(
        &self,
        tenant: StoredTenant,
        prediction: &MediaPredictionId,
    ) -> Result<Option<PredictionRow>, MediaStoreError> {
        Ok(sqlx::query_file_as!(
            PredictionRow,
            "src/store/predictions/by_prediction.sql",
            tenant as StoredTenant,
            prediction.as_str(),
        )
        .fetch_optional(self.pool())
        .await?)
    }

    /// The runs of `tasks`, in no particular order. A Task without a run is left out.
    pub async fn runs(
        &self,
        tenant: StoredTenant,
        tasks: &[TaskId],
    ) -> Result<Vec<PredictionRow>, MediaStoreError> {
        let ids: Vec<Uuid> = tasks.iter().map(|task| task.as_uuid()).collect();
        Ok(sqlx::query_file_as!(
            PredictionRow,
            "src/store/predictions/by_tasks.sql",
            tenant as StoredTenant,
            &ids,
        )
        .fetch_all(self.pool())
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

    /// One batch of runs recorded before `cutoff`, across tenants, oldest first, after
    /// `after`.
    pub async fn past_retention(
        &self,
        cutoff: DateTime<Utc>,
        after: Option<&AgedRun>,
    ) -> Result<Vec<AgedRun>, MediaStoreError> {
        let rows = sqlx::query_file!(
            "src/store/predictions/past_retention.sql",
            cutoff,
            after.map(|run| run.created_at),
            after.map(|run| run.task_id.as_uuid()),
            SWEEP_BATCH,
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| AgedRun {
                tenant: row.tenant,
                task_id: TaskId::from_uuid(row.task_id),
                created_at: row.created_at,
            })
            .collect())
    }

    /// Delete one run, which the sweep found without a Task.
    pub async fn delete_run(
        &self,
        tenant: StoredTenant,
        task_id: TaskId,
    ) -> Result<bool, MediaStoreError> {
        let deleted = sqlx::query_file!(
            "src/store/predictions/delete_run.sql",
            tenant as StoredTenant,
            task_id.as_uuid(),
        )
        .execute(self.pool())
        .await?;
        Ok(deleted.rows_affected() == 1)
    }
}
