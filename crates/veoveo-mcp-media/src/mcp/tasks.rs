//! `run` as a durable Task. Starting it checks the model and the input against the
//! catalog, so a bad request fails at once, then creates the Task in core's Task runtime
//! and records the run for the caller's prediction listing. Reading, updating, cancelling
//! and listening to Tasks go through the SDK's adapter under the caller as the owner.
use std::{collections::BTreeSet, sync::Arc, time::Duration};

use chrono::Utc;
use rmcp::{
    ErrorData as McpError, RoleServer,
    model::{
        CallToolRequestParams, CreateTaskResult, GetTaskParams, GetTaskResult, UpdateTaskParams,
    },
    service::RequestContext,
};
use veoveo_sdk_rust::{
    GatewayInternalIdentity,
    durable_tasks::{
        DurableTaskService, DurableTaskSubscription, get_durable_task, subscribe_durable_tasks,
        task_seed, update_durable_task,
    },
    hosting::gateway_identity,
};
use veoveo_task_runtime::{CreateTask, RecoveryClass, TaskRuntime, TaskStatus};
use veoveo_types::TaskId;

use crate::{
    catalog::{Catalog, InputError},
    contract::RunRequest,
    run::RUN,
    store::{MediaStore, RunEnding},
};

use super::{
    errors::{catalog_error, internal, store_error},
    identity,
    requirements::{Target, authorize},
};

/// How often a client should poll a run's Task.
const POLL_INTERVAL_MS: u64 = 3_000;

/// Media's durable Tasks.
#[derive(Clone)]
pub struct MediaTasks {
    runtime: TaskRuntime,
    store: MediaStore,
    catalog: Arc<Catalog>,
    retention: Duration,
}

impl MediaTasks {
    /// `retention` is how long a run's Task stays readable.
    pub fn new(
        runtime: TaskRuntime,
        store: MediaStore,
        catalog: Arc<Catalog>,
        retention: Duration,
    ) -> Self {
        Self {
            runtime,
            store,
            catalog,
            retention,
        }
    }
}

impl DurableTaskService for MediaTasks {
    type Caller = GatewayInternalIdentity;

    fn authenticate(&self, context: &RequestContext<RoleServer>) -> Result<Self::Caller, McpError> {
        gateway_identity(context)
    }

    async fn start_tool_task(
        &self,
        caller: &Self::Caller,
        request: CallToolRequestParams,
    ) -> Result<Option<CreateTaskResult>, McpError> {
        if request.name != "run" {
            return Ok(None);
        }
        authorize(caller, Target::Tool("run"))?;
        let arguments = serde_json::Value::Object(request.arguments.unwrap_or_default());
        let run: RunRequest = serde_json::from_value(arguments)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let snapshot = self.catalog.snapshot().map_err(catalog_error)?;
        snapshot
            .admit_input(&run.model, &run.input)
            .map_err(|error| match error {
                InputError::Catalog(error) => catalog_error(error),
                invalid => McpError::invalid_params(invalid.to_string(), None),
            })?;
        let owner = identity::task_owner(caller)?;
        let reader = identity::reader(caller)?;
        let created = self
            .runtime
            .create(CreateTask {
                task_id: TaskId::new(),
                owner,
                server: self.runtime.server().clone(),
                task_type: RUN,
                request: serde_json::to_value(&run).map_err(|error| internal(&error))?,
                recovery_class: RecoveryClass::Resume,
                idempotency_key: None,
                ttl_ms: Some(u64::try_from(self.retention.as_millis()).unwrap_or(u64::MAX)),
                poll_interval_ms: Some(POLL_INTERVAL_MS),
                retention_pins: BTreeSet::new(),
            })
            .await
            .map_err(|error| internal(&error))?;
        self.store
            .record_run(&reader, created.snapshot.task_id, &run.model, Utc::now())
            .await
            .map_err(store_error)?;
        Ok(Some(CreateTaskResult::new(task_seed(&created.snapshot))))
    }

    async fn get_task(
        &self,
        caller: &Self::Caller,
        request: GetTaskParams,
    ) -> Result<GetTaskResult, McpError> {
        get_durable_task(
            &self.runtime,
            &self.runtime.for_owner(&identity::task_owner(caller)?),
            request,
        )
        .await
    }

    async fn update_task(
        &self,
        caller: &Self::Caller,
        request: UpdateTaskParams,
    ) -> Result<(), McpError> {
        update_durable_task(
            &self.runtime.for_owner(&identity::task_owner(caller)?),
            request,
        )
        .await
    }

    /// A run cancelled before a worker claimed it never reached the provider: the
    /// runtime cancels it at once, and its prediction ends with it. A claimed or parked
    /// run reaches its wait, which carries the cancellation to the provider and ends the
    /// prediction.
    ///
    /// The caller's ownership is checked through the owner's query, and the request goes
    /// through the server's cancellation, which leaves a parked Task cancel-requested.
    /// Core's owner cancellation statement at 50e72a3 cancels an unowned parked Task
    /// outright, which its `task_parked` constraint refuses.
    async fn cancel_task(&self, caller: &Self::Caller, task_id: String) -> Result<(), McpError> {
        let unknown = || McpError::invalid_params("unknown task id", None);
        let task = veoveo_task_runtime::parse_task_id(&task_id).map_err(|_| unknown())?;
        self.runtime
            .for_owner(&identity::task_owner(caller)?)
            .get(task)
            .await
            .map_err(|error| internal(&error))?
            .ok_or_else(unknown)?;
        let snapshot = self
            .runtime
            .cancel(task)
            .await
            .map_err(|error| internal(&error))?;
        if snapshot.status == TaskStatus::Cancelled {
            self.store
                .finish(
                    identity::reader(caller)?.tenant(),
                    task,
                    None,
                    &RunEnding::Cancelled(Vec::new()),
                    Utc::now(),
                )
                .await
                .map_err(store_error)?;
        }
        Ok(())
    }

    async fn subscribe_tasks(
        &self,
        caller: &Self::Caller,
        task_ids: Vec<String>,
    ) -> Result<DurableTaskSubscription, McpError> {
        subscribe_durable_tasks(
            &self.runtime,
            &self.runtime.for_owner(&identity::task_owner(caller)?),
            task_ids,
        )
        .await
    }
}
