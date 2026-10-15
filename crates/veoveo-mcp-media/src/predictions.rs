//! Predictions as callers see them. A prediction is a `run` Task in core's Task runtime,
//! read through the runtime's owner API, beside Media's own row of the run. The Task
//! decides who sees the run and holds its status, failure and times; the row adds the
//! model, the provider's prediction, how the outcome arrives and the stored outputs.
//! Nothing in Media copies the Task's state, so a prediction always shows its Task's.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use veoveo_store::{CursorScope, KeysetCursor};
use veoveo_task::{
    TaskError, TaskOwner, TaskPageCursor, TaskPageOrder, TaskRuntime, TaskSnapshot, TaskStatus,
};
use veoveo_types::TaskId;

use crate::{
    contract::{
        GeneratedArtifact, ListCursor, MediaPredictionId, PredictionStatus, PredictionSummary,
        RunFailureCode, RunRequest,
    },
    run::{RUN, STORING_MESSAGE},
    store::{AgedRun, MediaStore, MediaStoreError, PredictionRow, stored_tenant},
};

/// The most predictions one listing page holds.
pub const PREDICTIONS_PER_PAGE: usize = 100;

/// Why a prediction read failed.
#[derive(Debug, thiserror::Error)]
pub enum PredictionError {
    #[error("the cursor belongs to another listing; start from the first page")]
    StaleCursor,
    #[error(transparent)]
    Store(#[from] MediaStoreError),
    #[error(transparent)]
    Tasks(#[from] TaskError),
}

/// One prediction: its Task and Media's row of the run, which a Task whose row was not
/// yet recorded lacks.
#[derive(Debug, Clone)]
pub struct Prediction {
    pub task: TaskSnapshot,
    pub row: Option<PredictionRow>,
}

impl Prediction {
    /// The run's status, from its Task. Media names the storing phase in the Task's
    /// status message.
    pub fn status(&self) -> PredictionStatus {
        let task = &self.task;
        match task.status {
            TaskStatus::Queued => PredictionStatus::Queued,
            TaskStatus::Running if task.status_message.as_deref() == Some(STORING_MESSAGE) => {
                PredictionStatus::Storing
            }
            TaskStatus::Running | TaskStatus::Waiting => PredictionStatus::Processing,
            TaskStatus::CancelRequested => PredictionStatus::Cancelling,
            TaskStatus::Succeeded => PredictionStatus::Succeeded,
            TaskStatus::Cancelled => PredictionStatus::Cancelled,
            TaskStatus::Failed
                if task.error.as_ref().is_some_and(|error| {
                    error.code == RunFailureCode::GenerationUnresolved.as_str()
                }) =>
            {
                PredictionStatus::Unresolved
            }
            TaskStatus::Failed => PredictionStatus::Failed,
        }
    }

    /// The provider's prediction, once the provider accepted the run.
    pub fn prediction_id(&self) -> Result<Option<MediaPredictionId>, MediaStoreError> {
        self.row
            .as_ref()
            .map(PredictionRow::prediction_id)
            .transpose()
            .map(Option::flatten)
    }

    /// The outputs the run stored in Artifact.
    pub fn outputs(&self) -> &[GeneratedArtifact] {
        self.row.as_ref().map(PredictionRow::outputs).unwrap_or(&[])
    }

    pub fn summary(&self) -> Result<PredictionSummary, MediaStoreError> {
        let status = self.status();
        let model_id = match &self.row {
            Some(row) => row.model_id()?,
            None => serde_json::from_value::<RunRequest>(self.task.request.clone())
                .map(|request| request.model)
                .map_err(|_| MediaStoreError::Corrupt("the Task's request"))?,
        };
        Ok(PredictionSummary {
            task_id: self.task.task_id,
            prediction_id: self.prediction_id()?,
            model_id,
            status,
            completion: self
                .row
                .as_ref()
                .map(PredictionRow::completion)
                .transpose()?
                .flatten(),
            error: (status == PredictionStatus::Failed)
                .then(|| self.task.error.as_ref().map(|error| error.message.clone()))
                .flatten(),
            output_count: u32::try_from(self.outputs().len())
                .map_err(|_| MediaStoreError::Corrupt("outputs"))?,
            created_at: self.task.created_at,
            completed_at: self.task.completed_at,
        })
    }
}

/// A page of predictions and the cursor of the next one.
#[derive(Debug, Clone)]
pub struct PredictionPage {
    pub items: Vec<Prediction>,
    pub next: Option<ListCursor>,
}

/// Media's predictions over core's Task runtime and Media's store.
#[derive(Clone)]
pub struct Predictions {
    tasks: TaskRuntime,
    store: MediaStore,
}

impl Predictions {
    pub fn new(tasks: TaskRuntime, store: MediaStore) -> Self {
        Self { tasks, store }
    }

    pub fn store(&self) -> &MediaStore {
        &self.store
    }

    /// One of `owner`'s predictions by the provider's identity. A prediction the owner may
    /// not see reads as missing.
    pub async fn prediction(
        &self,
        owner: &TaskOwner,
        prediction: &MediaPredictionId,
    ) -> Result<Option<Prediction>, PredictionError> {
        let tenant = stored_tenant(&owner.tenant())?;
        let Some(row) = self.store.by_prediction(tenant, prediction).await? else {
            return Ok(None);
        };
        let task = self
            .tasks
            .for_owner(owner)
            .of_type(RUN)
            .get(row.task_id())
            .await?;
        Ok(task.map(|task| Prediction {
            task,
            row: Some(row),
        }))
    }

    /// One of `owner`'s predictions by its Task.
    pub async fn by_task(
        &self,
        owner: &TaskOwner,
        task: TaskId,
    ) -> Result<Option<Prediction>, PredictionError> {
        let Some(task) = self.tasks.for_owner(owner).of_type(RUN).get(task).await? else {
            return Ok(None);
        };
        let row = self
            .store
            .run(stored_tenant(&owner.tenant())?, task.task_id)
            .await?;
        Ok(Some(Prediction { task, row }))
    }

    /// One page of `owner`'s predictions, newest first, after `cursor`.
    pub async fn page(
        &self,
        owner: &TaskOwner,
        cursor: Option<&ListCursor>,
    ) -> Result<PredictionPage, PredictionError> {
        let scope = CursorScope::new(
            "media.predictions",
            owner.tenant(),
            &owner.principal_key.to_string(),
        )
        .map_err(|_| PredictionError::StaleCursor)?;
        let after = cursor
            .map(|wire| {
                scope
                    .parse::<TaskPageCursor>(wire.as_str())
                    .map(|cursor: KeysetCursor<TaskPageCursor>| cursor.position().clone())
            })
            .transpose()
            .map_err(|_| PredictionError::StaleCursor)?;
        let page = self
            .tasks
            .for_owner(owner)
            .of_type(RUN)
            .page(
                TaskPageOrder::NewestFirst,
                after.as_ref(),
                PREDICTIONS_PER_PAGE,
            )
            .await
            .map_err(|error| match error {
                TaskError::PageOrderMismatch { .. } => PredictionError::StaleCursor,
                error => PredictionError::Tasks(error),
            })?;
        let ids: Vec<TaskId> = page.items.iter().map(|task| task.task_id).collect();
        let mut rows: BTreeMap<TaskId, PredictionRow> = self
            .store
            .runs(stored_tenant(&owner.tenant())?, &ids)
            .await?
            .into_iter()
            .map(|row| (row.task_id(), row))
            .collect();
        let items = page
            .items
            .into_iter()
            .map(|task| {
                let row = rows.remove(&task.task_id);
                Prediction { task, row }
            })
            .collect();
        let next = page
            .next_cursor
            .map(|next| {
                let cursor = scope
                    .cursor(next)
                    .map_err(|_| PredictionError::StaleCursor)?;
                ListCursor::parse(cursor.as_str()).map_err(|_| PredictionError::StaleCursor)
            })
            .transpose()?;
        Ok(PredictionPage { items, next })
    }

    /// Delete the rows of runs recorded before `cutoff` whose Task core's Task runtime no
    /// longer holds, across tenants, and return how many it deleted. A row outlives its
    /// Task by at most one sweep and never the other way round.
    pub async fn prune(&self, cutoff: DateTime<Utc>) -> Result<u64, PredictionError> {
        let mut deleted = 0;
        let mut after: Option<AgedRun> = None;
        loop {
            let batch = self.store.past_retention(cutoff, after.as_ref()).await?;
            let Some(last) = batch.last().copied() else {
                return Ok(deleted);
            };
            let ids: Vec<TaskId> = batch.iter().map(|run| run.task_id).collect();
            let held = self.held(&ids).await?;
            for run in &batch {
                if !held.contains(&run.task_id)
                    && self.store.delete_run(run.tenant, run.task_id).await?
                {
                    deleted += 1;
                }
            }
            after = Some(last);
        }
    }

    /// Which of `tasks` core's Task runtime still holds for this server, in one read.
    async fn held(&self, tasks: &[TaskId]) -> Result<BTreeSet<TaskId>, PredictionError> {
        Ok(self
            .tasks
            .get_many(tasks)
            .await?
            .into_iter()
            .map(|task| task.task_id)
            .collect())
    }
}
