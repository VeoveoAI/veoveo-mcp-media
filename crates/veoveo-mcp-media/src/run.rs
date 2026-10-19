//! A `run` Task's work, from its start or from where it parked. The Task checks the input
//! against the model's schema, reserves the estimated cost, hands the run to the provider
//! as an operation and parks. Under events completion the submission carries a callback
//! URL and the provider's signed webhook settles the operation; under checks completion
//! it carries none and bounded checks of the prediction's status endpoint settle it. The
//! resumed Task streams each output into Artifact as one durable step, reconciles the
//! provider's bill through a second operation with bounded checks, records the actual
//! cost, and completes with links to the stored outputs.
use std::{collections::BTreeSet, io, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use chrono::Utc;
use futures::TryStreamExt as _;
use serde_json::{Map, json};
use tokio_util::sync::CancellationToken;
use url::Url;
use veoveo_hub_sdk::{
    definition::Product,
    durable_tasks::{TaskContext, TaskStop, TaskWorkError, TaskWorker, mcp_task_completion},
    hosting::task_result,
    usage::{
        Amount, ChargePeriod, Currency, Money, Quantity, ServiceCategory, UsageKind,
        UsageKindDeclaration, UsageKinds, UsageUnit,
    },
};
use veoveo_mcp_artifact::{
    client::{ArtifactClient, UploadBody},
    contract::{
        ArtifactLabels, ArtifactMetadataMap, ArtifactName, ArtifactResource, IdempotencyKey,
        MediaType, ObjectSize, RetentionDays,
    },
};
use veoveo_store::TenantId as StoredTenant;
use veoveo_task::{
    CheckObservation, CheckSchedule, Completion, ExternalJobId, Operation, OperationCalls,
    OperationSpec, StepDispatch, StepKey, Submission, TaskError, TaskFailure, TaskTransition,
    WaitOutcome,
};
use veoveo_types::{ResourceAddress, TaskId};

use crate::{
    catalog::{Catalog, InputError},
    contract::{
        CompletionMode, GENERATION_COMPLETED, GENERATION_FORMAT, GeneratedArtifact,
        GenerationResult, MediaPredictionId, MediaResource, PredictionStatus, RunFailureCode,
        RunRequest,
    },
    provider::{
        BillingOutcome, ProviderClient, ProviderError, billing_key, billing_provider,
        cancel_prediction, check_billing, check_status, generation_key, generation_provider,
        prediction_of,
        wire::{Outcome, Prediction},
    },
    store::{MediaStore, MediaStoreError, stored_tenant},
};

/// The Task operation of `run`.
pub const RUN: veoveo_types::TaskTypeName = veoveo_types::TaskTypeName::from_static("media.run");

/// Media's usage: one record per generation, in US dollars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaUsage {
    Generation,
}

impl UsageKinds for MediaUsage {
    fn all() -> &'static [Self] {
        &[Self::Generation]
    }

    fn declaration(self) -> UsageKindDeclaration {
        match self {
            Self::Generation => UsageKindDeclaration {
                kind: UsageKind::parse("media.generation").expect("Media's usage kind"),
                service_category: ServiceCategory::AiAndMachineLearning,
                unit: UsageUnit::requests(),
                monetary: true,
            },
        }
    }
}

/// How a run learns its generation's outcome. The installation's configuration chooses
/// it: a public origin selects webhooks to Media's callback path on it, and its absence
/// selects bounded status checks. Media never falls back from one to the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunCompletion {
    /// The provider posts the outcome to the run's callback URL.
    Events,
    /// The run sends no callback URL and checks the prediction's status endpoint on this
    /// schedule until the outcome is final or the deadline passes.
    Checks(CheckSchedule),
}

impl RunCompletion {
    /// The status-check schedule of a run without a callback: first after 3 seconds,
    /// then every 3 seconds growing by a quarter each time, at most 40 checks.
    pub fn status_checks() -> Self {
        Self::Checks(
            CheckSchedule::new(Duration::from_secs(3), Duration::from_secs(3), 1.25, 40)
                .expect("a valid status schedule"),
        )
    }
}

/// How runs wait, store outputs and reconcile bills.
#[derive(Debug, Clone)]
pub struct RunSettings {
    /// How a run learns its generation's outcome.
    pub completion: RunCompletion,
    /// How long a run waits for the provider's outcome before it is unresolved.
    pub generation_timeout: Duration,
    /// When the bill is checked after the outcome: first, between checks, the backoff and
    /// how many checks at most.
    pub billing_schedule: CheckSchedule,
    /// How long billing is reconciled before the run completes without an actual cost.
    pub billing_timeout: Duration,
    /// The most bytes one output may hold.
    pub output_max_bytes: u64,
    /// How long fetching and storing one output may take.
    pub output_timeout: Duration,
    /// The hosts outputs may be fetched from, as `host` or `host:port`. Empty admits the
    /// provider's own host only.
    pub output_hosts: BTreeSet<String>,
    /// How long a run waits for Artifact to verify a stored output.
    pub verification_timeout: Duration,
    /// How long Artifact keeps a run's outputs, when the installation bounds it.
    pub output_retention: Option<RetentionDays>,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            completion: RunCompletion::status_checks(),
            generation_timeout: Duration::from_secs(2 * 60 * 60),
            billing_schedule: CheckSchedule::new(
                Duration::from_secs(5),
                Duration::from_secs(10),
                2.0,
                8,
            )
            .expect("a valid billing schedule"),
            billing_timeout: Duration::from_secs(30 * 60),
            output_max_bytes: 2 * 1024 * 1024 * 1024,
            output_timeout: Duration::from_secs(15 * 60),
            output_hosts: BTreeSet::new(),
            verification_timeout: Duration::from_secs(10 * 60),
            output_retention: None,
        }
    }
}

/// How many redirects one output fetch follows.
const OUTPUT_REDIRECTS: usize = 3;

/// The hosts outputs may be fetched from: the configured ones, or else the provider's.
#[derive(Debug, Clone)]
struct OutputHosts {
    configured: BTreeSet<String>,
    provider: Option<(String, String)>,
}

impl OutputHosts {
    /// Whether `url` is an HTTP or HTTPS URL on an allowed host.
    fn admit(&self, url: &Url) -> bool {
        let Some((host, with_port)) = authority(url) else {
            return false;
        };
        let allowed = if self.configured.is_empty() {
            self.provider
                .as_ref()
                .is_some_and(|(provider, provider_with_port)| {
                    *provider == host || *provider_with_port == with_port
                })
        } else {
            self.configured.contains(&host) || self.configured.contains(&with_port)
        };
        allowed && matches!(url.scheme(), "http" | "https")
    }
}

/// What every `run` Task shares. It also makes the provider calls a run's waits need:
/// status checks, billing checks and cancellations.
#[derive(Clone)]
pub struct MediaWork {
    pub store: MediaStore,
    pub catalog: Arc<Catalog>,
    pub provider: ProviderClient,
    /// Moves outputs' bytes to object storage. It never carries the provider's API key.
    pub http: reqwest::Client,
    pub settings: Arc<RunSettings>,
    output_hosts: OutputHosts,
    /// Fetches outputs. It follows a redirect only to an allowed host.
    outputs: reqwest::Client,
}

impl MediaWork {
    /// The work of Media's runs. Outputs are fetched through a client of their own,
    /// which follows at most three redirects, each to an allowed host.
    pub fn new(
        store: MediaStore,
        catalog: Arc<Catalog>,
        provider: ProviderClient,
        http: reqwest::Client,
        settings: Arc<RunSettings>,
    ) -> anyhow::Result<Self> {
        let output_hosts = OutputHosts {
            configured: settings.output_hosts.clone(),
            provider: provider.host(),
        };
        let redirects = output_hosts.clone();
        let outputs = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > OUTPUT_REDIRECTS || !redirects.admit(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .context("building the output client")?;
        Ok(Self {
            store,
            catalog,
            provider,
            http,
            settings,
            output_hosts,
            outputs,
        })
    }

    /// The generation operation of a run under the configured completion.
    fn generation(&self) -> OperationSpec {
        match self.settings.completion {
            RunCompletion::Events => OperationSpec::events(
                generation_key(),
                generation_provider(),
                self.settings.generation_timeout,
            ),
            RunCompletion::Checks(schedule) => OperationSpec::checks(
                generation_key(),
                generation_provider(),
                schedule,
                self.settings.generation_timeout,
            ),
        }
    }
}

impl OperationCalls for MediaWork {
    type Error = anyhow::Error;

    async fn check(&self, operation: &Operation) -> Result<CheckObservation, Self::Error> {
        if operation.key == generation_key() {
            let status_url = self
                .store
                .status_url(operation.task_id)
                .await?
                .ok_or_else(|| anyhow!("the provider named no status endpoint for the run"))?;
            check_status(&self.provider, &status_url).await
        } else if operation.key == billing_key() {
            check_billing(&self.provider, &prediction_of(operation)?).await
        } else {
            Err(anyhow!("Media checks no operation `{}`", operation.key))
        }
    }

    /// A cancellation reaches the provider for a running generation. A billing check has
    /// nothing to cancel at the provider.
    async fn cancel(&self, operation: &Operation) -> Result<serde_json::Value, Self::Error> {
        if operation.key == billing_key() {
            return Ok(json!({ "reconciled": false }));
        }
        cancel_prediction(&self.provider, operation).await
    }
}

/// Why a run failed, with the code its Task records.
struct RunFailure {
    code: RunFailureCode,
    message: String,
}

impl RunFailure {
    fn new(code: RunFailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The failure of a run that stopped on an error none of its steps handled. The
    /// message names the cause Media recognizes and no secret: provider and store
    /// errors name no URL, and anything else keeps its details to Media's log.
    fn unexpected(error: &anyhow::Error) -> Self {
        if let Some(MediaStoreError::PredictionConflict(prediction)) =
            error.downcast_ref::<MediaStoreError>()
        {
            return Self::new(
                RunFailureCode::PredictionConflict,
                format!(
                    "the provider named prediction {prediction}, which another run already holds"
                ),
            );
        }
        let stored = error.chain().any(|cause| {
            cause.is::<sqlx::Error>()
                || cause.is::<MediaStoreError>()
                || cause.downcast_ref::<TaskError>().is_some_and(|error| {
                    matches!(error, TaskError::Database(_) | TaskError::Store(_))
                })
        });
        if stored {
            Self::new(
                RunFailureCode::StoreRefused,
                "the installation database refused to record the run's progress",
            )
        } else {
            Self::new(
                RunFailureCode::RunError,
                "the run stopped on an unexpected error; Media's log holds its details",
            )
        }
    }
}

/// The stored tenant of a run's row, from its Task's owner.
fn tenant(ctx: &TaskContext) -> anyhow::Result<StoredTenant> {
    stored_tenant(&ctx.task().owner.tenant()).context("the Task names no stored tenant")
}

/// The Task's status message while Media stores a run's outputs, which names the run's
/// storing phase.
pub const STORING_MESSAGE: &str = "Storing the outputs";

/// An amount of US dollars, to the micro-dollar, never negative: a refund larger than
/// its deduction nets to zero.
fn usd(amount: f64) -> anyhow::Result<Money> {
    let fixed = format!("{:.6}", amount.max(0.0));
    let trimmed = fixed.trim_end_matches('0').trim_end_matches('.');
    Ok(Money {
        amount: Amount::parse(if trimmed.is_empty() { "0" } else { trimmed })?,
        currency: Currency::parse("USD")?,
    })
}

/// One `run` Task's work. An error none of its steps handled fails the run under a
/// code that names its cause, unless the Task parked, stopped with its process, left
/// this worker or lost its database connection: those keep the Task for its wait or its
/// recovery. The Task alone records how the run ended.
pub async fn run(ctx: TaskContext, media: MediaWork) -> anyhow::Result<TaskTransition> {
    match attempt(&ctx, &media).await {
        Err(error) if !keeps_the_task(&error) && !ctx.stopping() => {
            let failure = RunFailure::unexpected(&error);
            tracing::warn!(
                task = %ctx.task_id(),
                code = %failure.code,
                error = %format!("{error:#}"),
                "a run failed on an unexpected error"
            );
            Ok(fail(failure))
        }
        outcome => outcome,
    }
}

async fn attempt(ctx: &TaskContext, media: &MediaWork) -> anyhow::Result<TaskTransition> {
    let request: RunRequest = serde_json::from_value(ctx.task().request.clone())
        .context("the Task's request is no run request")?;
    let tenant = tenant(ctx)?;
    let task_id = ctx.task_id();
    media
        .store
        .record_run(tenant, task_id, &request.model, ctx.task().created_at)
        .await?;
    let generation = media.generation();
    // Before the run reaches the provider: check its input, stop on a cancellation and
    // reserve its estimated cost. A run resumed after its dispatch skips all three.
    let dispatched = ctx.runtime().operation(task_id, &generation.key).await?;
    if dispatched.is_none() {
        if let Err(failure) = admit(media, &request) {
            return Ok(fail(failure));
        }
        match ctx.checkpoint().await {
            Ok(()) => {}
            Err(TaskStop::Cancelled) => return Ok(TaskTransition::Cancelled),
            Err(stop) => return Err(stop.into()),
        }
        // The provider's quote for this input is the estimate. It is a durable step, so
        // a run resumed before its dispatch records the same estimate again, which the
        // usage ledger takes as a repeat.
        let (provider, model, input) = (&media.provider, &request.model, &request.input);
        let quote = ctx
            .step(&quote_key(), |_| async move {
                provider
                    .quote(model, input)
                    .await
                    .map_err(|error| anyhow::Error::from(Unpriced(error)))
            })
            .await;
        let price: f64 = match quote {
            Ok(price) => price,
            Err(error) => match error.downcast::<Unpriced>() {
                Ok(Unpriced(error)) => {
                    let failure = RunFailure::new(
                        RunFailureCode::PriceUnavailable,
                        format!("the provider could not quote the run's price: {error}"),
                    );
                    return Ok(fail(failure));
                }
                Err(error) => return Err(error),
            },
        };
        // A quote is an external await: cancellation may arrive while it is held.
        // Stop before any dispatch; never add this check between dispatch and ctx.wait.
        match ctx.checkpoint().await {
            Ok(()) => {}
            Err(TaskStop::Cancelled) => return Ok(TaskTransition::Cancelled),
            Err(stop) => return Err(stop.into()),
        }
        ctx.usage()?
            .estimate(MediaUsage::Generation, Quantity::whole(1))
            .money(usd(price)?)
            .operation(&generation.key)
            .await?;
    }
    // Hand the run to the provider once. A submission whose outcome is unknown waits
    // for the provider's report instead of submitting again. A run under events
    // completion sends its callback URL; a run under checks completion sends none.
    let provider = media.provider.clone();
    let store = media.store.clone();
    let model = request.model.clone();
    let input = request.input.clone();
    let operation = ctx
        .operation(&generation)
        .dispatch(|dispatch| async move {
            match dispatch.dispatch {
                StepDispatch::First => {
                    match provider
                        .submit(&model, &input, dispatch.callback_url.as_ref())
                        .await
                    {
                        Ok(prediction) => {
                            store
                                .record_submission(
                                    tenant,
                                    task_id,
                                    Some(&prediction.id),
                                    prediction.urls.as_ref().map(|urls| urls.get.as_str()),
                                    None,
                                )
                                .await?;
                            Ok(Submission::job(ExternalJobId::parse(prediction.id.as_str())?))
                        }
                        Err(error) if error.is_ambiguous() => {
                            tracing::warn!(%error, "a submission's outcome is unknown; waiting for the provider's webhook");
                            Ok(Submission::handed_over())
                        }
                        Err(error) => Err(anyhow::Error::from(Refused(error))),
                    }
                }
                // The earlier attempt may have reached the provider, which offers no
                // lookup by Media's identity: wait for its webhook.
                StepDispatch::Unsettled { .. } => Ok(Submission::handed_over()),
            }
        })
        .await;
    let operation = match operation {
        Ok(operation) => operation,
        Err(error) => match error.downcast::<Refused>() {
            Ok(Refused(refusal)) => {
                let failure = RunFailure::new(
                    RunFailureCode::ProviderRefused,
                    format!("the provider refused the run: {refusal}"),
                );
                return Ok(fail(failure));
            }
            Err(error) => return Err(error),
        },
    };
    let prediction_id = operation
        .external_id
        .as_ref()
        .and_then(|job| MediaPredictionId::parse(job.as_str()).ok());
    // The operation records how its outcome arrives; a run resumed under another
    // configuration keeps the mode it was dispatched with.
    let completion = completion_mode(&operation.completion);
    media
        .store
        .record_submission(
            tenant,
            task_id,
            prediction_id.as_ref(),
            None,
            Some(completion),
        )
        .await?;
    // Park until the provider's outcome arrives. Nothing checks for cancellation before
    // this wait, so a cancellation reaches the provider through it.
    let outcome = ctx.wait::<Prediction, _>(&generation.key, media).await?;
    let prediction = match outcome {
        WaitOutcome::Settled(prediction) => prediction,
        WaitOutcome::Expired => {
            return Ok(fail(RunFailure::new(
                RunFailureCode::GenerationUnresolved,
                "the provider reported no outcome before the deadline",
            )));
        }
        WaitOutcome::Cancelled { .. } => return Ok(TaskTransition::Cancelled),
    };
    match prediction.outcome() {
        Some(Outcome::Completed) => {}
        other => {
            reconcile_billing(ctx, media, &prediction.id).await?;
            let reason = prediction
                .error
                .clone()
                .filter(|error| !error.trim().is_empty())
                .unwrap_or_else(|| match other {
                    Some(Outcome::TimedOut) => "the provider timed out".into(),
                    Some(Outcome::Deleted) => "the provider deleted the prediction".into(),
                    Some(Outcome::Cancelled) => "the provider cancelled the prediction".into(),
                    _ => "the provider failed the prediction".into(),
                });
            return Ok(fail(RunFailure::new(
                RunFailureCode::GenerationFailed,
                reason,
            )));
        }
    }
    ctx.update(TaskTransition::Running {
        message: STORING_MESSAGE.into(),
        progress: 0.5,
    })
    .await?;
    let mut outputs = Vec::with_capacity(prediction.outputs.len());
    for (index, url) in prediction.outputs.iter().enumerate() {
        let key = StepKey::parse(format!("output-{index}"))?;
        let stored = ctx
            .step(&key, |_| {
                store_output(ctx, media, task_id, &prediction, index, url)
            })
            .await;
        match stored {
            Ok(stored) => outputs.push(stored),
            Err(error) if is_task_stop(&error) => return Err(error),
            Err(error) => {
                reconcile_billing(ctx, media, &prediction.id).await?;
                return Ok(fail(RunFailure::new(
                    RunFailureCode::OutputNotStored,
                    format!("output {index} could not be stored: {error:#}"),
                )));
            }
        }
    }
    reconcile_billing(ctx, media, &prediction.id).await?;
    let result = GenerationResult {
        format: GENERATION_FORMAT.into(),
        task_id,
        prediction: crate::contract::PredictionSummary {
            task_id,
            prediction_id: Some(prediction.id.clone()),
            model_id: prediction.model.clone(),
            status: PredictionStatus::Succeeded,
            completion: Some(completion),
            error: None,
            output_count: u32::try_from(outputs.len()).unwrap_or(u32::MAX),
            created_at: ctx.task().created_at,
            completed_at: Some(Utc::now()),
        },
        artifacts: outputs.clone(),
    };
    media
        .store
        .record_outputs(tenant, task_id, &prediction.id, &outputs)
        .await?;
    let product = Product::new(&MediaResource::Result(prediction.id.clone()), result)?;
    let completion =
        task_result::<RunRequest>(&product).map_err(|error| anyhow!(error.message.to_string()))?;
    Ok(mcp_task_completion(GENERATION_COMPLETED, completion)?)
}

/// How an operation's outcome arrives, as a run's prediction names it.
fn completion_mode(completion: &Completion) -> CompletionMode {
    match completion {
        Completion::Events => CompletionMode::Events,
        Completion::Checks(_) => CompletionMode::Checks,
    }
}

/// The durable step that holds the provider's quote for the run.
fn quote_key() -> StepKey {
    StepKey::parse("quote").expect("a valid step key")
}

/// A provider's failure to quote a run's price, before the run reached it.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct Unpriced(ProviderError);

/// A provider refusal of a submission: the run reached the provider, which declined it.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct Refused(ProviderError);

fn is_task_stop(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TaskStop>().is_some() || keeps_the_task(error)
}

/// Whether `error` leaves the Task to the worker as it stands: the Task parked on an
/// operation, stopped at a checkpoint, no longer belongs to this instance, or lost its
/// database connection, which says nothing about the run and sends the Task to its
/// recovery.
fn keeps_the_task(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TaskStop>().is_some()
        || TaskWorkError::is_parked(error)
        || TaskWorkError::is_lost(error)
        || TaskWorkError::is_interrupted(error)
}

/// Admit the run: the catalog holds the model, and the input satisfies its schema.
fn admit(media: &MediaWork, request: &RunRequest) -> Result<(), RunFailure> {
    let snapshot = media
        .catalog
        .snapshot()
        .map_err(|error| RunFailure::new(RunFailureCode::CatalogUnavailable, error.to_string()))?;
    match snapshot.admit_input(&request.model, &request.input) {
        Ok(_) => Ok(()),
        Err(InputError::Catalog(error)) => Err(RunFailure::new(
            RunFailureCode::UnknownModel,
            error.to_string(),
        )),
        Err(error) => Err(RunFailure::new(
            RunFailureCode::InvalidInput,
            error.to_string(),
        )),
    }
}

/// The Task's failure under the code and message that name its cause.
fn fail(failure: RunFailure) -> TaskTransition {
    TaskTransition::Failed(TaskFailure::new(failure.code.as_str(), failure.message))
}

/// Reconcile the prediction's bill through bounded checks and record the actual cost. A
/// bill that does not appear in time leaves the estimate as the run's cost.
async fn reconcile_billing(
    ctx: &TaskContext,
    media: &MediaWork,
    prediction: &MediaPredictionId,
) -> anyhow::Result<()> {
    let billing = OperationSpec::checks(
        billing_key(),
        billing_provider(),
        media.settings.billing_schedule,
        media.settings.billing_timeout,
    );
    let job = ExternalJobId::parse(prediction.as_str())?;
    ctx.operation(&billing)
        .dispatch(|_| async move { Ok::<_, anyhow::Error>(Submission::job(job)) })
        .await?;
    match ctx.wait::<BillingOutcome, _>(&billing.key, media).await? {
        WaitOutcome::Settled(bill) => {
            if let Some(net) = bill.net_usd() {
                let mut actual = ctx
                    .usage()?
                    .actual(MediaUsage::Generation, Quantity::whole(1))
                    .money(usd(net)?)
                    .operation(&billing.key);
                if let Some(billed_at) = bill
                    .records
                    .iter()
                    .filter_map(|record| record.created_at)
                    .max()
                {
                    actual = actual.period(ChargePeriod::at(billed_at));
                }
                actual.await?;
            }
        }
        WaitOutcome::Expired => {
            tracing::warn!(%prediction, "the provider's bill did not appear before the deadline");
        }
        WaitOutcome::Cancelled { .. } => {}
    }
    Ok(())
}

/// The host and port a URL names, as allowed output hosts spell them.
fn authority(url: &Url) -> Option<(String, String)> {
    let host = url.host_str()?.to_ascii_lowercase();
    let with_port = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.clone(),
    };
    Some((host, with_port))
}

/// The media type an output's response names, without parameters.
fn media_type(response: &reqwest::Response, url: &Url) -> MediaType {
    let named = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    let guessed = || {
        let extension = url.path().rsplit('.').next()?.to_ascii_lowercase();
        Some(
            match extension.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                "gif" => "image/gif",
                "mp4" => "video/mp4",
                "webm" => "video/webm",
                "mp3" => "audio/mpeg",
                "wav" => "audio/wav",
                "glb" => "model/gltf-binary",
                _ => return None,
            }
            .to_owned(),
        )
    };
    named
        .filter(|value| value != "application/octet-stream")
        .or_else(guessed)
        .and_then(|value| MediaType::parse(value).ok())
        .unwrap_or_else(|| {
            MediaType::parse("application/octet-stream").expect("a valid media type")
        })
}

/// A URL path segment with its percent-encoding decoded, when it decodes to UTF-8.
fn decoded(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = segment.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// An output's artifact name: the last segment of its URL, or `output-{index}`.
fn output_name(url: &Url, index: usize) -> ArtifactName {
    url.path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .and_then(decoded)
        .and_then(|segment| ArtifactName::parse(segment).ok())
        .unwrap_or_else(|| {
            ArtifactName::parse(format!("output-{index}")).expect("a valid artifact name")
        })
}

/// Fetch one output and stream it into Artifact for the run's caller. The fetch is
/// bounded in time and size and reaches only allowed hosts, and no output is held in
/// memory whole.
async fn store_output(
    ctx: &TaskContext,
    media: &MediaWork,
    task_id: TaskId,
    prediction: &Prediction,
    index: usize,
    url: &str,
) -> anyhow::Result<GeneratedArtifact> {
    let url = Url::parse(url).context("the provider named an output that is no URL")?;
    let settings = &media.settings;
    let (_, with_port) =
        authority(&url).ok_or_else(|| anyhow!("the output's URL names no host"))?;
    if !media.output_hosts.admit(&url) {
        return Err(anyhow!("the output's host {with_port} is not allowed"));
    }
    let response = media
        .outputs
        .get(url.clone())
        .timeout(settings.output_timeout)
        .send()
        .await
        .map_err(|error| anyhow!("fetching the output failed: {}", error.without_url()))?;
    let status = response.status();
    if status.is_redirection() {
        return Err(anyhow!(
            "the output redirected off the allowed hosts or past {OUTPUT_REDIRECTS} redirects"
        ));
    }
    if !status.is_success() {
        return Err(anyhow!("fetching the output answered HTTP {status}"));
    }
    if response
        .content_length()
        .is_some_and(|length| length > settings.output_max_bytes)
    {
        return Err(anyhow!(
            "the output exceeds {} bytes",
            settings.output_max_bytes
        ));
    }
    let media_type = media_type(&response, &url);
    let name = output_name(&url, index);
    let body = UploadBody::stream(
        response
            .bytes_stream()
            .map_err(|error| io::Error::other(error.without_url())),
    );
    let mut metadata = Map::new();
    metadata.insert("taskId".into(), json!(task_id.to_string()));
    metadata.insert("predictionId".into(), json!(prediction.id.as_str()));
    metadata.insert("modelId".into(), json!(prediction.model.as_str()));
    metadata.insert("outputIndex".into(), json!(index));
    let labels = ArtifactLabels::new(ctx.task().owner.data_labels.clone())
        .map_err(|error| anyhow!("the run's labels: {error}"))?;
    let artifacts = ArtifactClient::new(ctx.client()?, media.http.clone());
    let mut upload = artifacts
        .upload(name, media_type.clone(), body)
        .max_size(ObjectSize::new(settings.output_max_bytes).map_err(|error| anyhow!("{error}"))?)
        .idempotency_key(
            IdempotencyKey::parse(format!("media:{task_id}:output:{index}"))
                .map_err(|error| anyhow!("{error}"))?,
        )
        .labels(labels)
        .metadata(ArtifactMetadataMap::new(metadata).map_err(|error| anyhow!("{error}"))?)
        .until_verified(settings.verification_timeout);
    if let Some(days) = settings.output_retention {
        upload = upload.retention(days);
    }
    let record = upload.await.context("storing the output in Artifact")?;
    Ok(GeneratedArtifact {
        index: u32::try_from(index).unwrap_or(u32::MAX),
        artifact_uri: ArtifactResource::artifact(record.artifact_id)
            .to_uri()
            .map_err(|error| anyhow!("{error}"))?,
        media_type: media_type.as_str().to_owned(),
        size: record.size.map(ObjectSize::bytes),
    })
}

/// Run Media's generations on `worker` until `stop`.
pub async fn work(
    worker: TaskWorker,
    media: MediaWork,
    stop: CancellationToken,
) -> Result<(), TaskError> {
    worker
        .run(
            move |ctx| {
                let media = media.clone();
                async move { run(ctx, media).await }
            },
            stop,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_named_and_typed_from_their_urls() {
        let url = Url::parse("https://cdn.example/out/image%201.png?sig=x").unwrap();
        assert_eq!(output_name(&url, 0).as_str(), "image 1.png");
        let bare = Url::parse("https://cdn.example/").unwrap();
        assert_eq!(
            output_name(&Url::parse("https://cdn.example/%ff").unwrap(), 1).as_str(),
            "output-1"
        );
        assert_eq!(output_name(&bare, 3).as_str(), "output-3");
        assert_eq!(
            authority(&Url::parse("http://Fake.Provider:8803/x").unwrap()),
            Some(("fake.provider".into(), "fake.provider:8803".into()))
        );
    }

    #[test]
    fn money_is_never_negative_and_keeps_at_most_six_places() {
        assert_eq!(usd(0.04).unwrap().amount.as_str(), "0.04");
        assert_eq!(usd(1.0).unwrap().amount.as_str(), "1");
        assert_eq!(usd(0.1234567).unwrap().amount.as_str(), "0.123457");
        assert_eq!(usd(-1.0).unwrap().amount.as_str(), "0");
    }
}
