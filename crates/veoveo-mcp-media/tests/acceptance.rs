//! Acceptance through a running local installation: Media registered with the Hub,
//! called through the Hub by the installation's member and admin callers, with each
//! output stored in Artifact for the caller and its bytes read from the installation's
//! object store through the URL Artifact signs.
//!
//! Live cases are ignored by default. Use the native HTTPS `VEOVEO_ACCEPTANCE_HUB`
//! and prepared `VEOVEO_ACCEPTANCE_{MEMBER,ADMIN}_BEARER_ENV` selectors, whose
//! distinct variables hold bearer/opaque references. No caller signing keys are
//! copied. Fake-provider cases need the testing feature, the private controlled fake,
//! and exact fixture/provenance inputs described in README. Run serially, excluding
//! `restart_persistence_` and `a_running_generation_recovers_after_media_and_hub_restart`
//! from the ordinary suite. The explicit restart phases need independently verified
//! native lifecycle evidence; this file holds no deployment or management authority.
//!
//! One case bills the real provider once and runs only with
//! `MEDIA_PROVIDER_QUALIFICATION=1`, against Media configured for the real provider:
//!
//! | Variable | Meaning |
//! |---|---|
//! | `MEDIA_QUALIFICATION_MODEL` | The model to run, such as the one `tests/provider_qualification.rs` quotes lowest |
//! | `MEDIA_QUALIFICATION_INPUT` | The model's input as a JSON object; by default a short prompt |
//! | `MEDIA_QUALIFICATION_COMPLETION` | How the installation's Media learns outcomes: `events` with a public origin, `checks` without |
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities,
        ClientConfig, DetailedTask, ErrorCode, GetTaskParams, Implementation, ProtocolVersion,
        ReadResourceRequestParams, ResourceContents, ServerNotification, SubscriptionFilter,
        TaskPayload, TaskStatus,
    },
    service::{RunningService, ServiceError, Subscription},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use veoveo_hub_sdk::client::{BearerSource as _, StaticBearer};
use veoveo_mcp_artifact::contract::{
    ArtifactEntry, ArtifactLabels, CreateDownloadResult, SignedTransfer, TransferMethod,
};
use veoveo_mcp_media::{
    contract::{
        CompletionMode, GenerationResult, MediaPredictionId, ModelCatalogPage, ModelSchema,
        PredictionStatus, PredictionView,
    },
    fake::{
        FakeObservation, IMAGE_BYTES, IMAGE_MODEL, LARGE_BYTES, LARGE_MODEL, PRICE_USD, output_byte,
    },
};
use veoveo_types::{PrincipalId, Sha256Digest, WorkContextId};

#[derive(Clone, Copy)]
enum CallerRole {
    Member,
    Admin,
}

struct AcceptanceBearers {
    member: StaticBearer,
    admin: Option<StaticBearer>,
}

impl AcceptanceBearers {
    /// Injectable lookup keeps input regression tests independent of process state.
    fn load(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        ensure!(
            lookup("VEOVEO_ACCEPTANCE_KEY_DIRECTORY").is_none(),
            "VEOVEO_ACCEPTANCE_KEY_DIRECTORY is retired; supply prepared bearer inputs"
        );
        let member_env = lookup("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV")
            .context("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV must name the member bearer variable")?;
        let admin_env = lookup("VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV");
        ensure!(
            admin_env.as_ref() != Some(&member_env),
            "member and admin bearer selectors must name distinct variables"
        );
        let member = Self::read(&lookup, &member_env, "member")?;
        let admin = admin_env
            .as_deref()
            .map(|name| Self::read(&lookup, name, "admin"))
            .transpose()?;
        ensure!(
            admin.as_ref() != Some(&member),
            "member and admin bearer inputs must be distinct"
        );
        Ok(Self {
            member: StaticBearer::new(member),
            admin: admin.map(StaticBearer::new),
        })
    }

    fn read(lookup: &impl Fn(&str) -> Option<String>, name: &str, role: &str) -> Result<String> {
        ensure!(
            !name.is_empty()
                && name.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                }),
            "{role} bearer selector must be an environment variable name"
        );
        let value = lookup(name).with_context(|| format!("{role} bearer variable is missing"))?;
        ensure!(
            !value.is_empty() && !value.chars().any(char::is_whitespace),
            "{role} bearer variable must contain a nonempty bearer without whitespace"
        );
        Ok(value)
    }

    fn source(&self, role: CallerRole) -> Result<&StaticBearer> {
        match role {
            CallerRole::Member => Ok(&self.member),
            CallerRole::Admin => self
                .admin
                .as_ref()
                .context("VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV is required for admin qualification"),
        }
    }
}

const MEMBER: CallerRole = CallerRole::Member;
const ADMIN: CallerRole = CallerRole::Admin;

struct Caller {
    tasks: bool,
}

impl ClientHandler for Caller {
    fn get_info(&self) -> ClientConfig {
        let capabilities = if self.tasks {
            ClientCapabilities::builder().enable_tasks().build()
        } else {
            ClientCapabilities::default()
        };
        ClientConfig::new(
            capabilities,
            Implementation::new("veoveo-mcp-media-acceptance", env!("CARGO_PKG_VERSION")),
        )
    }
}

type Client = RunningService<RoleClient, Caller>;

struct Installation {
    hub: url::Url,
    bearers: AcceptanceBearers,
    http: reqwest::Client,
}

impl Installation {
    fn from_env() -> Result<Self> {
        // reqwest is built without a TLS provider; Media uses rustls with ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let hub = std::env::var("VEOVEO_ACCEPTANCE_HUB")
            .context("VEOVEO_ACCEPTANCE_HUB names the Hub origin")?;
        let bearers = AcceptanceBearers::load(|name| std::env::var(name).ok())?;
        // Preserve ordinary certificate/name verification and provisioned native roots.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            hub: url::Url::parse(&hub)?,
            bearers,
            http,
        })
    }

    fn mcp(&self) -> Result<url::Url> {
        Ok(self.hub.join("/mcp/local")?)
    }

    async fn token(&self, role: CallerRole) -> Result<String> {
        Ok(self
            .bearers
            .source(role)?
            .caller_bearer()
            .await?
            .expose()
            .to_owned())
    }

    async fn download(&self, transfer: &SignedTransfer) -> Result<bytes::Bytes> {
        ensure!(
            transfer.method == TransferMethod::Get,
            "Artifact must sign a GET"
        );
        let mut request = self.http.get(transfer.url.as_str());
        for (name, value) in &transfer.headers {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(reqwest::Error::without_url)?;
        ensure!(
            response.status().is_success(),
            "signed download answered {}",
            response.status()
        );
        response
            .bytes()
            .await
            .map_err(reqwest::Error::without_url)
            .context("reading signed download bytes")
    }

    /// Media's generation usage in `caller`'s own daily cost view at the Hub, from
    /// yesterday through tomorrow.
    async fn generation_usage(&self, caller: CallerRole) -> Result<GenerationUsage> {
        let token = self.token(caller).await?;
        let today = chrono::Utc::now().date_naive();
        let mut totals = GenerationUsage::default();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = self.hub.join("/admin/local/usage/daily")?;
            {
                let mut query = url.query_pairs_mut();
                query
                    .append_pair("scope", "own")
                    .append_pair("from", &(today - chrono::TimeDelta::days(1)).to_string())
                    .append_pair("to", &(today + chrono::TimeDelta::days(1)).to_string());
                if let Some(cursor) = &cursor {
                    query.append_pair("cursor", cursor);
                }
            }
            let response = self.http.get(url).bearer_auth(&token).send().await?;
            let status = response.status();
            ensure!(status.is_success(), "the cost view answered {status}");
            let page: Value = response.json().await?;
            for row in page["rows"].as_array().context("the cost view's rows")? {
                if row["server"] != "media" || row["kind"] != "media.generation" {
                    continue;
                }
                let records = row["records"].as_u64().unwrap_or(0);
                match row["phase"].as_str() {
                    Some("estimate") => {
                        totals.estimates += records;
                        totals.estimate_usd += amount(&row["money"]).unwrap_or(0.0);
                    }
                    Some("actual") => {
                        totals.actuals += records;
                        totals.actual_usd += amount(&row["money"]).unwrap_or(0.0);
                    }
                    _ => {}
                }
            }
            match page["nextCursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return Ok(totals),
            }
        }
    }

    async fn connect(&self, caller: CallerRole) -> Result<Client> {
        self.connect_with_tasks(caller, true).await
    }

    async fn connect_with_tasks(&self, caller: CallerRole, tasks: bool) -> Result<Client> {
        let token = self.token(caller).await?;
        let transport = StreamableHttpClientTransport::with_client(
            self.http.clone(),
            StreamableHttpClientTransportConfig::with_uri(self.mcp()?.to_string())
                .auth_header(token),
        );
        Ok(Caller { tasks }
            .serve_with_lifecycle(
                transport,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await?)
    }
}

fn tool(server: &str, name: &str, arguments: Value) -> Result<CallToolRequestParams> {
    let arguments = arguments
        .as_object()
        .cloned()
        .context("tool arguments are an object")?;
    Ok(CallToolRequestParams::new(format!("{server}__{name}")).with_arguments(arguments))
}

async fn call<T: DeserializeOwned>(
    client: &Client,
    server: &str,
    name: &str,
    arguments: Value,
) -> Result<T> {
    let result = client.call_tool(tool(server, name, arguments)?).await?;
    ensure!(
        !result.is_error.unwrap_or(false),
        "{server}__{name} returned an error"
    );
    serde_json::from_value(result.structured_content.context("structured result")?)
        .with_context(|| format!("{name} returned a result of another shape"))
}

async fn read<T: DeserializeOwned>(client: &Client, uri: &str) -> Result<T> {
    let result = client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await?;
    let text = result
        .contents
        .iter()
        .find_map(|content| match content {
            ResourceContents::TextResourceContents { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .context("text resource")?;
    serde_json::from_str(text).with_context(|| format!("{uri} holds another shape"))
}

/// Media's generation usage in a daily cost view: how many estimate and actual records,
/// and the actual records' US dollars.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct GenerationUsage {
    estimates: u64,
    estimate_usd: f64,
    actuals: u64,
    actual_usd: f64,
}

/// The US dollars of a money object.
fn amount(money: &Value) -> Option<f64> {
    (money["currency"] == "USD")
        .then(|| money["amount"].as_str()?.parse().ok())
        .flatten()
}

/// The usage records a Task view carries in its `ai.veoveo/usage` metadata.
fn task_usage(view: &Value) -> Vec<Value> {
    match view {
        Value::Object(map) => map
            .get("ai.veoveo/usage")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| map.values().flat_map(task_usage).collect()),
        Value::Array(items) => items.iter().flat_map(task_usage).collect(),
        _ => Vec::new(),
    }
}

/// Start `run` of `model` with `input` as a Task and wait up to `deadline` for it to
/// finish.
async fn generate(
    client: &Client,
    model: &str,
    input: Value,
    deadline: Duration,
) -> Result<(String, Value)> {
    let request = tool("media", "run", json!({"model": model, "input": input}))?;
    let created = match client.call_tool_once(request).await? {
        CallToolResponse::Task(created) => created.task,
        other => bail!("run did not create a Task: {other:?}"),
    };
    let task_id = created.task_id.clone();
    let finished = tokio::time::timeout(deadline, async {
        loop {
            let task = client.get_task(GetTaskParams::new(&task_id)).await?;
            match task.task.status() {
                TaskStatus::Working => tokio::time::sleep(Duration::from_millis(500)).await,
                TaskStatus::Completed => return Ok::<_, anyhow::Error>(task),
                status => bail!("the run reported {status:?}"),
            }
        }
    })
    .await
    .with_context(|| format!("the run did not finish within {deadline:?}"))??;
    let view = serde_json::to_value(&finished)?;
    let TaskPayload::Completed { result } = &finished.task.payload else {
        bail!("the completed run carries no result")
    };
    let structured = result
        .get("structuredContent")
        .cloned()
        .context("the result has no structured content")?;
    Ok((task_id, json!({"result": structured, "view": view})))
}

#[tokio::test]
#[ignore = "requires native fake-provider controls and an eligible billed caller"]
async fn a_run_through_the_hub_stores_its_output_in_artifact_with_its_cost() -> Result<()> {
    qualify_output(IMAGE_MODEL, IMAGE_BYTES).await.map(|_| ())
}

#[tokio::test]
#[ignore = "requires native fake-provider controls and an eligible billed caller"]
async fn a_large_output_streams_through_artifact_with_exact_bytes() -> Result<()> {
    qualify_output(LARGE_MODEL, LARGE_BYTES).await.map(|_| ())
}

#[tokio::test]
#[ignore = "requires a running local installation with the fake provider"]
async fn a_member_browses_models_and_is_refused_the_billed_run() -> Result<()> {
    let installation = Installation::from_env()?;
    let member = installation.connect(MEMBER).await?;
    let models: ModelCatalogPage = call(&member, "media", "models", json!({})).await?;
    ensure!(!models.models.is_empty(), "the member reads the catalog");
    let request = tool(
        "media",
        "run",
        json!({"model": IMAGE_MODEL, "input": {"prompt": "x"}}),
    )?;
    match member.call_tool_once(request).await {
        Err(ServiceError::McpError(error)) => {
            ensure!(
                error.code == ErrorCode::INVALID_REQUEST
                    && error
                        .message
                        .starts_with("You don't have permission to call `media__run`.")
                    && error
                        .message
                        .contains("Your roles don't grant the permission this requires."),
                "run failed with a different MCP error, not the expected Hub billed-permission refusal"
            );
            Ok(())
        }
        Ok(_) => bail!("run was served to the member"),
        Err(error) => Err(error).context("run failed without a refusal"),
    }
}

#[tokio::test]
#[ignore = "bills the real provider; set MEDIA_PROVIDER_QUALIFICATION=1 with Media configured for it"]
async fn a_real_provider_run_through_the_hub_stores_its_output_and_records_its_costs() -> Result<()>
{
    if std::env::var("MEDIA_PROVIDER_QUALIFICATION").as_deref() != Ok("1") {
        eprintln!("MEDIA_PROVIDER_QUALIFICATION is not 1; the real-provider case does not run");
        return Ok(());
    }
    let installation = Installation::from_env()?;
    let model = std::env::var("MEDIA_QUALIFICATION_MODEL")
        .context("MEDIA_QUALIFICATION_MODEL names the model to run")?;
    let input = match std::env::var("MEDIA_QUALIFICATION_INPUT") {
        Ok(input) => serde_json::from_str(&input)
            .context("MEDIA_QUALIFICATION_INPUT is the model's input as a JSON object")?,
        Err(_) => json!({"prompt": "A small red circle centered on a plain white background"}),
    };
    let expected: CompletionMode = std::env::var("MEDIA_QUALIFICATION_COMPLETION")
        .context("MEDIA_QUALIFICATION_COMPLETION names the configured completion")?
        .parse()
        .map_err(|_| anyhow!("MEDIA_QUALIFICATION_COMPLETION is events or checks"))?;
    let admin = installation.connect(ADMIN).await?;
    let before = installation.generation_usage(ADMIN).await?;

    let (task_id, finished) = generate(&admin, &model, input, Duration::from_secs(30 * 60)).await?;
    let result: GenerationResult = serde_json::from_value(finished["result"].clone())?;
    ensure!(!result.artifacts.is_empty(), "the run stored its outputs");

    // Every output is the caller's in Artifact, and its bytes download in full.
    for output in &result.artifacts {
        let artifact_id = output
            .artifact_uri
            .as_str()
            .strip_prefix("artifact://artifacts/")
            .context("an Artifact address")?;
        let download: CreateDownloadResult = call(
            &admin,
            "artifact",
            "create_download",
            json!({"artifactId": artifact_id}),
        )
        .await?;
        let bytes = installation.download(&download.get).await?;
        ensure!(!bytes.is_empty(), "an output holds bytes");
        ensure!(
            output.size == Some(bytes.len() as u64),
            "the output keeps its size"
        );
    }

    // The outcome arrived the way the installation's configuration chose.
    let prediction = result
        .prediction
        .prediction_id
        .clone()
        .context("the provider's prediction")?;
    let view: PredictionView = read(&admin, &format!("media://prediction/{prediction}")).await?;
    ensure!(
        view.prediction.completion == Some(expected)
            && result.prediction.completion == Some(expected),
        "the run completed through {:?}, not {expected}",
        view.prediction.completion
    );

    // The usage ledger holds the run's estimate and its actual cost, and the Task
    // carries the actual one.
    let after = installation.generation_usage(ADMIN).await?;
    ensure!(
        after.estimates == before.estimates + 1 && after.actuals == before.actuals + 1,
        "the cost view gained one estimate and one actual record: {before:?} then {after:?}"
    );
    let actual = task_usage(&finished["view"])
        .into_iter()
        .find(|record| record["kind"] == "media.generation" && record["phase"] == "actual")
        .context("the Task carries the run's actual cost")?;
    let actual_usd = amount(&actual["money"]).context("the actual cost in US dollars")?;
    ensure!(
        (after.actual_usd - before.actual_usd - actual_usd).abs() < 1e-9,
        "the ledger's actual cost matches the Task's"
    );
    eprintln!(
        "run {task_id}: {model} completed through {expected}, stored {} output(s), cost {actual_usd} USD",
        result.artifacts.len()
    );
    Ok(())
}

/// Every fixture condition is observed, with a deadline; delays only pace HTTP observations.
impl Installation {
    fn fixture(&self, path: &str) -> Result<url::Url> {
        let origin = url::Url::parse(&std::env::var("VEOVEO_ACCEPTANCE_MEDIA_FIXTURE").context(
            "VEOVEO_ACCEPTANCE_MEDIA_FIXTURE names the native HTTPS fake-provider origin",
        )?)?;
        ensure!(
            origin.scheme() == "https"
                && origin.has_host()
                && origin.username().is_empty()
                && origin.password().is_none()
                && origin.path() == "/"
                && origin.query().is_none()
                && origin.fragment().is_none(),
            "fixture must be an HTTPS origin without credentials, path, query or fragment"
        );
        Ok(origin.join(path)?)
    }

    async fn control(&self, path: &str, body: Value) -> Result<()> {
        let response = self
            .http
            .post(self.fixture(path)?)
            .json(&body)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        ensure!(
            response.status().is_success(),
            "fixture control answered {}",
            response.status()
        );
        Ok(())
    }

    async fn observation(&self) -> Result<FakeObservation> {
        let response = self
            .http
            .get(self.fixture("/testing/state")?)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        ensure!(
            response.status().is_success(),
            "fixture observation answered {}",
            response.status()
        );
        Ok(response.json().await.map_err(reqwest::Error::without_url)?)
    }

    async fn observed(&self, ready: impl Fn(&FakeObservation) -> bool) -> Result<FakeObservation> {
        bounded(30, async {
            loop {
                let observation = self.observation().await?;
                if ready(&observation) {
                    return Ok(observation);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .context("waiting for an explicit fixture condition")
    }

    async fn controlled(&self) -> Result<()> {
        self.control("/testing/quotes", json!({"hold":false}))
            .await?;
        self.control("/testing/delivery", json!({"automatic":false}))
            .await
    }

    async fn finish_prediction(&self, prediction: &MediaPredictionId) -> Result<()> {
        self.control(
            &format!("/testing/predictions/{prediction}/complete"),
            json!({"deliver":false}),
        )
        .await
    }
}

async fn bounded<T>(seconds: u64, run: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(seconds), run)
        .await
        .with_context(|| format!("qualification exceeded {seconds} seconds"))?
}

async fn start_run(client: &Client, model: &str) -> Result<String> {
    match client
        .call_tool_once(tool(
            "media",
            "run",
            json!({"model":model,"input":{"prompt":"native deterministic qualification"}}),
        )?)
        .await?
    {
        CallToolResponse::Task(created) => Ok(created.task.task_id),
        _ => bail!("media__run did not create a Task"),
    }
}

async fn next_task(listener: &mut Subscription, task_id: &str) -> Result<DetailedTask> {
    loop {
        let notification = listener
            .next()
            .await?
            .with_context(|| format!("Task {task_id} subscription ended: {:?}", listener.end()))?;
        if let ServerNotification::TaskStatusNotification(notification) = notification {
            ensure!(
                notification.params.task.task.task_id == task_id,
                "notification names another Task"
            );
            return Ok(notification.params.task);
        }
    }
}

async fn working(client: &Client, task_id: &str) -> Result<Subscription> {
    let filter = SubscriptionFilter::builder().task_id(task_id).build();
    let mut listener = client.listen(filter.clone()).await?;
    ensure!(
        listener.acknowledged() == &filter,
        "Task filter was not acknowledged exactly"
    );
    ensure!(
        next_task(&mut listener, task_id).await?.status() == TaskStatus::Working,
        "the Task subscription must open on Working"
    );
    Ok(listener)
}

async fn terminal(
    listener: &mut Subscription,
    task_id: &str,
    expected: TaskStatus,
) -> Result<DetailedTask> {
    loop {
        let task = next_task(listener, task_id).await?;
        if task.status() == TaskStatus::Working {
            continue;
        }
        ensure!(
            task.status() == expected,
            "Task reported {:?}, expected {expected:?}",
            task.status()
        );
        return Ok(task);
    }
}

fn generation(task: &DetailedTask) -> Result<GenerationResult> {
    let TaskPayload::Completed { result } = &task.payload else {
        bail!("Task has no completed result")
    };
    Ok(serde_json::from_value(
        result
            .get("structuredContent")
            .cloned()
            .context("generation structured content")?,
    )?)
}

async fn dispatched(
    installation: &Installation,
    before: &FakeObservation,
) -> Result<MediaPredictionId> {
    let observed = installation
        .observed(|state| {
            state.submissions > before.submissions && state.status_reads > before.status_reads
        })
        .await?;
    ensure!(
        observed.submissions == before.submissions + 1,
        "provider received more than one submission"
    );
    let mut ids = observed.predictions.iter().filter(|prediction| {
        !before
            .predictions
            .iter()
            .any(|prior| prior.prediction_id == prediction.prediction_id)
    });
    let id = ids
        .next()
        .context("fixture received no new prediction")?
        .prediction_id
        .clone();
    ensure!(
        ids.next().is_none(),
        "fixture received more than one prediction"
    );
    Ok(id)
}

async fn verify_output(
    installation: &Installation,
    client: &Client,
    result: &GenerationResult,
    model: &str,
    length: u64,
) -> Result<()> {
    ensure!(
        result.format == veoveo_mcp_media::contract::GENERATION_FORMAT
            && result.artifacts.len() == 1,
        "one typed output must belong to the generation format"
    );
    ensure!(
        result.prediction.model_id.as_str() == model
            && result.prediction.completion == Some(CompletionMode::Checks),
        "the expected model must complete through configured status checks"
    );
    let prediction = result
        .prediction
        .prediction_id
        .as_ref()
        .context("provider prediction")?;
    let artifact = &result.artifacts[0];
    ensure!(
        artifact.index == 0 && artifact.size == Some(length),
        "output index/size differs"
    );
    let entry: ArtifactEntry = read(client, artifact.artifact_uri.as_str()).await?;
    let owner = PrincipalId::parse(
        std::env::var("VEOVEO_ACCEPTANCE_BILLED_PRINCIPAL")
            .context("VEOVEO_ACCEPTANCE_BILLED_PRINCIPAL names the admitted billed caller")?,
    )?;
    let context = WorkContextId::parse(
        std::env::var("VEOVEO_ACCEPTANCE_WORK_CONTEXT").unwrap_or_else(|_| "local".into()),
    )?;
    let labels: ArtifactLabels = serde_json::from_str(
        &std::env::var("VEOVEO_ACCEPTANCE_LABELS").unwrap_or_else(|_| "[]".into()),
    )?;
    let record = &entry.artifact;
    ensure!(
        record.owner == owner
            && record.provenance.caller == owner
            && record.work_context == context
            && record.labels == labels
            && record
                .provenance
                .acting_server
                .as_ref()
                .map(|slug| slug.as_str())
                == Some("media")
            && record.provenance.task.as_ref().map(|id| id.as_str())
                == Some(result.task_id.to_string().as_str()),
        "Artifact owner/context/labels/Task delegation provenance differs from the admitted caller"
    );
    ensure!(
        record.metadata.as_object().get("taskId") == Some(&json!(result.task_id.to_string()))
            && record.metadata.as_object().get("predictionId") == Some(&json!(prediction.as_str()))
            && record.metadata.as_object().get("modelId") == Some(&json!(model))
            && record.metadata.as_object().get("outputIndex") == Some(&json!(0)),
        "output provenance metadata differs"
    );
    let response = client
        .call_tool(tool(
            "artifact",
            "create_download",
            json!({"artifactId":record.artifact_id}),
        )?)
        .await?;
    ensure!(
        !response.is_error.unwrap_or(false),
        "Artifact create_download returned a tool error"
    );
    let structured = response
        .structured_content
        .context("signed transfer structured content")?;
    let text_value = response
        .content
        .iter()
        .filter_map(|content| content.as_text())
        .find_map(|text| serde_json::from_str::<Value>(&text.text).ok())
        .context("signed transfer serialized JSON TextContent")?;
    // Do not use assert_eq: its debug output would reveal a signed URL on failure.
    ensure!(
        text_value == structured,
        "signed transfer JSON text differs from structuredContent"
    );
    let download: CreateDownloadResult = serde_json::from_value(text_value)
        .map_err(|_| anyhow!("signed transfer JSON has another shape"))?;
    let bytes = installation.download(&download.get).await?;
    ensure!(
        bytes.len() as u64 == length
            && bytes
                .iter()
                .enumerate()
                .all(|(offset, byte)| *byte == output_byte(offset as u64)),
        "the signed GET did not return the exact provider bytes"
    );
    use sha2::Digest as _;
    let digest = Sha256Digest::from_bytes(sha2::Sha256::digest(&bytes).into());
    ensure!(
        record.sha256.as_ref() == Some(&digest),
        "Artifact checksum differs from downloaded bytes"
    );
    let view: PredictionView = read(client, &format!("media://prediction/{prediction}")).await?;
    ensure!(
        view.prediction.task_id == result.task_id
            && view.prediction.status == PredictionStatus::Succeeded
            && view.prediction.output_count == 1,
        "prediction does not retain the unique completed output"
    );
    eprintln!(
        "Media Task {} prediction {prediction}: saved {length} bytes, sha256 {digest}",
        result.task_id
    );
    Ok(())
}

async fn cleanup_run(installation: &Installation, task_id: Option<&str>) {
    if let Some(task_id) = task_id {
        let _ = bounded(10, async {
            let caller = installation.connect(ADMIN).await?;
            caller.cancel_task(CancelTaskParams::new(task_id)).await?;
            Ok(())
        })
        .await;
    }
}

async fn qualify_output(model: &str, length: u64) -> Result<String> {
    let installation = Installation::from_env()?;
    let mut cleanup_task = None;
    let outcome = bounded(240, async {
        installation.controlled().await?;
        let before = installation.observation().await?;
        let usage_before = installation.generation_usage(ADMIN).await?;
        let caller = installation.connect(ADMIN).await?;
        let task_id = start_run(&caller, model).await?;
        cleanup_task = Some(task_id.clone());
        let mut listener = working(&caller, &task_id).await?;
        let prediction = dispatched(&installation, &before).await?;
        installation.finish_prediction(&prediction).await?;
        // No tasks/get between opening Working and this pushed completion.
        let completed = terminal(&mut listener, &task_id, TaskStatus::Completed).await?;
        let result = generation(&completed)?;
        ensure!(
            result.prediction.prediction_id.as_ref() == Some(&prediction),
            "Task completed another prediction"
        );
        verify_output(&installation, &caller, &result, model, length).await?;
        let retained = caller.get_task(GetTaskParams::new(&task_id)).await?.task;
        ensure!(
            retained.payload == completed.payload,
            "completion was not retained"
        );
        let after = installation.observation().await?;
        ensure!(
            after.submissions == before.submissions + 1
                && after.output_reads == before.output_reads + 1,
            "provider submission/output transfer was duplicated"
        );
        let usage_after = installation.generation_usage(ADMIN).await?;
        ensure!(
            usage_after.estimates == usage_before.estimates + 1
                && (usage_after.estimate_usd - usage_before.estimate_usd - PRICE_USD).abs() < 1e-9
                && usage_after.actuals == usage_before.actuals + 1
                && (usage_after.actual_usd - usage_before.actual_usd - PRICE_USD).abs() < 1e-9,
            "the daily ledger did not gain one quote and actual generation charge"
        );
        let actual = task_usage(&serde_json::to_value(&completed)?)
            .into_iter()
            .find(|record| record["kind"] == "media.generation" && record["phase"] == "actual")
            .context("Task actual generation usage")?;
        ensure!(
            actual["quantity"] == "1"
                && actual["unit"] == "Requests"
                && amount(&actual["money"]).is_some_and(|amount| (amount - PRICE_USD).abs() < 1e-9),
            "Task actual quantity/unit or charge differs from one fake generation"
        );
        Ok(task_id)
    })
    .await;
    if outcome.is_err() {
        cleanup_run(&installation, cleanup_task.as_deref()).await;
    }
    outcome
}

#[tokio::test]
#[ignore = "native fake-provider controls; run serially"]
async fn catalog_schema_and_task_only_invocation_are_preserved() -> Result<()> {
    bounded(60, async {
        let installation = Installation::from_env()?;
        let member = installation.connect(MEMBER).await?;
        let catalog: ModelCatalogPage = call(&member, "media", "models", json!({})).await?;
        ensure!(
            catalog
                .models
                .iter()
                .any(|model| model.model_id.as_str() == IMAGE_MODEL)
                && catalog
                    .models
                    .iter()
                    .any(|model| model.model_id.as_str() == LARGE_MODEL),
            "expected fake models are missing"
        );
        let schema: ModelSchema = call(
            &member,
            "media",
            "model_schema",
            json!({"model":IMAGE_MODEL}),
        )
        .await?;
        ensure!(
            schema.model_id.as_str() == IMAGE_MODEL
                && schema
                    .request_schema
                    .as_ref()
                    .is_some_and(|schema| schema["required"] == json!(["prompt"])),
            "model schema differs"
        );
        let ordinary = installation.connect_with_tasks(ADMIN, false).await?;
        let before = installation.observation().await?;
        match ordinary
            .call_tool(tool(
                "media",
                "run",
                json!({"model":IMAGE_MODEL,"input":{"prompt":"x"}}),
            )?)
            .await
        {
            Err(ServiceError::McpError(error)) => ensure!(
                error.code == ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY
                    && error
                        .data
                        .as_ref()
                        .is_some_and(|data| data["requiredCapabilities"]
                            == json!({
                                "extensions": {"io.modelcontextprotocol/tasks": {}}
                            })),
                "ordinary invocation was refused with another MCP error"
            ),
            Err(error) => {
                return Err(error)
                    .context("ordinary invocation failed without a Task-required refusal");
            }
            Ok(_) => bail!("ordinary call reached Task-only run"),
        }
        let after = installation.observation().await?;
        ensure!(
            after.quotes == before.quotes && after.submissions == before.submissions,
            "ordinary call quoted or dispatched provider work"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "native fixture held-quote barrier; run serially"]
async fn cancellation_during_a_held_quote_never_submits_to_the_provider() -> Result<()> {
    let installation = Installation::from_env()?;
    let mut cleanup_task = None;
    let outcome = bounded(120, async {
        installation.controlled().await?;
        let before = installation.observation().await?;
        installation
            .control("/testing/quotes", json!({"hold":true}))
            .await?;
        let caller = installation.connect(ADMIN).await?;
        let task_id = start_run(&caller, IMAGE_MODEL).await?;
        cleanup_task = Some(task_id.clone());
        let mut listener = working(&caller, &task_id).await?;
        installation
            .observed(|state| state.pending_quotes > before.pending_quotes)
            .await?;
        caller.cancel_task(CancelTaskParams::new(&task_id)).await?;
        installation
            .control("/testing/quotes", json!({"hold":false}))
            .await?;
        terminal(&mut listener, &task_id, TaskStatus::Cancelled).await?;
        let after = installation.observation().await?;
        ensure!(
            after.submissions == before.submissions
                && after.cancellations == before.cancellations
                && after.output_reads == before.output_reads,
            "cancelled quote dispatched or published output"
        );
        Ok(())
    })
    .await;
    // Deadline cancellation drops the probe future, so cleanup must live outside it.
    // Cancel our Task before releasing a quote if the probe stopped before its cancel RPC.
    if outcome.is_err() {
        cleanup_run(&installation, cleanup_task.as_deref()).await;
    }
    let released = bounded(
        10,
        installation.control("/testing/quotes", json!({"hold":false})),
    )
    .await;
    outcome.and(released)
}

#[tokio::test]
#[ignore = "native fixture controlled completion; run serially"]
async fn cancellation_after_dispatch_reaches_the_provider_without_publishing_output() -> Result<()>
{
    let installation = Installation::from_env()?;
    let mut cleanup_task = None;
    let outcome = bounded(120, async {
        installation.controlled().await?;
        let before = installation.observation().await?;
        let caller = installation.connect(ADMIN).await?;
        let task_id = start_run(&caller, IMAGE_MODEL).await?;
        cleanup_task = Some(task_id.clone());
        let mut listener = working(&caller, &task_id).await?;
        let prediction = dispatched(&installation, &before).await?;
        caller.cancel_task(CancelTaskParams::new(&task_id)).await?;
        terminal(&mut listener, &task_id, TaskStatus::Cancelled).await?;
        let after = installation.observed(|state| state.cancellations > before.cancellations).await?;
        ensure!(after.submissions == before.submissions + 1 && after.cancellations == before.cancellations + 1
            && after.output_reads == before.output_reads && after.predictions.iter().any(|entry| entry.prediction_id == prediction && entry.status == "cancelled"),
            "provider cancellation/submission/output evidence differs");
        let view: PredictionView = read(&caller, &format!("media://prediction/{prediction}")).await?;
        ensure!(view.prediction.status == PredictionStatus::Cancelled && view.prediction.output_count == 0,
            "cancelled prediction published output");
        eprintln!("Cancelled Hub Task {task_id}, Media Task {} prediction {prediction}; trusted operator verifies stored operation receipt", view.prediction.task_id);
        Ok(())
    }).await;
    if outcome.is_err() {
        cleanup_run(&installation, cleanup_task.as_deref()).await;
    }
    outcome
}

#[tokio::test]
#[ignore = "isolated external Media and Hub restart; exact continue line required"]
async fn a_running_generation_recovers_after_media_and_hub_restart() -> Result<()> {
    let installation = Installation::from_env()?;
    let mut cleanup_task = None;
    let outcome = bounded(360, async {
        installation.controlled().await?;
        let before = installation.observation().await?;
        let caller = installation.connect(ADMIN).await?;
        let task_id = start_run(&caller, IMAGE_MODEL).await?;
        cleanup_task = Some(task_id.clone());
        let initial = working(&caller, &task_id).await?;
        // A provider status read occurs only after the operation was durably dispatched and parked.
        let prediction = dispatched(&installation, &before).await?;
        drop(initial);
        caller.cancel().await?;
        println!("Working Task {task_id}, prediction {prediction}; ready for trusted Media and Hub restart; waiting for continue");
        std::io::Write::flush(&mut std::io::stdout()).context("flushing restart readiness")?;
        continue_after_restart().await?;

        let resumed = installation.connect(ADMIN).await?;
        let current = resumed.get_task(GetTaskParams::new(&task_id)).await
            .context("reading the same Task after the trusted restart boundary")?.task;
        ensure!(current.status() == TaskStatus::Working,
            "inconclusive restart evidence: the same Task is {:?}, not Working", current.status());
        let mut listener = working(&resumed, &task_id).await.context("reopening the Working Task subscription")?;
        let state = installation.observation().await?;
        ensure!(state.submissions == before.submissions + 1 && state.output_reads == before.output_reads
            && state.predictions.iter().any(|entry| entry.prediction_id == prediction && entry.status == "processing"),
            "the parked generation was resubmitted or completed before release");
        eprintln!("Task {task_id}: Working after verified restart, same provider prediction; waiting for pushed completion");
        installation.finish_prediction(&prediction).await?;
        let completed = terminal(&mut listener, &task_id, TaskStatus::Completed).await.context("waiting for pushed completion after restart")?;
        let result = generation(&completed)?;
        ensure!(result.prediction.prediction_id.as_ref() == Some(&prediction), "recovered result names another prediction");
        verify_output(&installation, &resumed, &result, IMAGE_MODEL, IMAGE_BYTES).await?;
        let fresh = installation.connect(ADMIN).await?;
        let retained = fresh.get_task(GetTaskParams::new(&task_id)).await?.task;
        ensure!(retained.payload == completed.payload, "recovered completion was not retained");
        let state = installation.observation().await?;
        ensure!(state.submissions == before.submissions + 1 && state.output_reads == before.output_reads + 1,
            "the recovered generation or output was duplicated");
        eprintln!("Task {task_id}: one independently observed submission, Working after restart, pushed Completed, one output and retained result");
        Ok(())
    }).await;
    if outcome.is_err()
        && let Some(task_id) = cleanup_task
    {
        let _ = bounded(10, async {
            let caller = installation.connect(ADMIN).await?;
            caller.cancel_task(CancelTaskParams::new(task_id)).await?;
            Ok(())
        })
        .await;
    }
    outcome
}

async fn continue_after_restart() -> Result<()> {
    let (send, receive) = tokio::sync::oneshot::channel();
    // A detached standard reader cannot keep a timed-out Tokio runtime alive.
    std::thread::Builder::new()
        .name("media-restart-continue".into())
        .spawn(move || {
            let mut input = std::io::Read::take(std::io::stdin().lock(), 64);
            let mut line = Vec::new();
            let result = std::io::BufRead::read_until(&mut input, b'\n', &mut line)
                .context("reading Media restart continuation")
                .and_then(|_| {
                    ensure!(
                        line == b"continue\n",
                        "restart continuation must be exactly continue followed by LF"
                    );
                    Ok(())
                });
            let _ = send.send(result);
        })
        .context("starting bounded restart continuation reader")?;
    receive.await.context("restart continuation reader ended")?
}

#[tokio::test]
#[ignore = "explicit terminal prepare phase before trusted Media and Hub restarts"]
async fn restart_persistence_prepare() -> Result<()> {
    let task_id = qualify_output(IMAGE_MODEL, IMAGE_BYTES).await?;
    println!("VEOVEO_MEDIA_RESTART_TASK_ID={task_id}");
    Ok(())
}

#[tokio::test]
#[ignore = "explicit terminal verify phase after trusted Media and Hub restarts"]
async fn restart_persistence_verify() -> Result<()> {
    bounded(120, async {
        let task_id = std::env::var("VEOVEO_MEDIA_RESTART_TASK_ID").context("VEOVEO_MEDIA_RESTART_TASK_ID from the prepare phase")?;
        let installation = Installation::from_env()?;
        let caller = installation.connect(ADMIN).await?;
        let retained = caller.get_task(GetTaskParams::new(&task_id)).await?.task;
        ensure!(retained.status() == TaskStatus::Completed, "prepared Task did not retain Completed");
        let result = generation(&retained)?;
        let filter = SubscriptionFilter::builder().task_id(&task_id).build();
        let mut listener = caller.listen(filter.clone()).await?;
        ensure!(listener.acknowledged() == &filter, "retained Task filter was not acknowledged");
        ensure!(next_task(&mut listener, &task_id).await?.payload == retained.payload,
            "fresh Task subscription did not open with retained completion");
        let stored: GenerationResult = read(&caller, &result.result_uri.to_string()).await?;
        let prediction_id = result.prediction.prediction_id.as_ref().context("retained provider prediction")?;
        let view: PredictionView = read(&caller, &format!("media://prediction/{prediction_id}")).await?;
        ensure!(result.prediction.completed_at.is_some() && stored.prediction.completed_at.is_some(),
            "Task result and persisted prediction must both retain completion timestamps");
        ensure!(stored.prediction == view.prediction,
            "result resource differs from the authoritative persisted prediction");
        // run.rs stamps the result before core commits the Task transition; predictions.rs
        // projects that transition's persisted timestamp. Only those completion times differ.
        let mut expected = result.clone();
        expected.prediction.completed_at = view.prediction.completed_at;
        ensure!(stored == expected, "retained prediction result differs from retained Task completion semantics");
        verify_output(&installation, &caller, &result, IMAGE_MODEL, IMAGE_BYTES).await?;
        eprintln!("Task {task_id}: terminal result, prediction, Artifact metadata/bytes and fresh subscription retained");
        Ok(())
    }).await
}

#[cfg(test)]
mod bearer_inputs {
    use super::*;
    use std::collections::BTreeMap;

    fn load(entries: &[(&str, &str)]) -> Result<AcceptanceBearers> {
        let values: BTreeMap<_, _> = entries.iter().copied().collect();
        AcceptanceBearers::load(|name| values.get(name).map(|value| (*value).to_owned()))
    }

    #[tokio::test]
    async fn selects_each_role_without_exposing_bearers() -> Result<()> {
        let bearers = load(&[
            ("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE"),
            ("VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV", "ADMIN_REFERENCE"),
            ("MEMBER_REFERENCE", "opaque-member-reference"),
            ("ADMIN_REFERENCE", "opaque-admin-reference"),
        ])?;
        assert_eq!(
            bearers.source(CallerRole::Member)?.caller_bearer().await?,
            veoveo_hub_sdk::client::Bearer::new("opaque-member-reference")
        );
        assert_eq!(
            bearers.source(CallerRole::Admin)?.caller_bearer().await?,
            veoveo_hub_sdk::client::Bearer::new("opaque-admin-reference")
        );
        Ok(())
    }

    #[test]
    fn member_only_input_does_not_require_admin_custody() -> Result<()> {
        let bearers = load(&[
            ("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE"),
            ("MEMBER_REFERENCE", "opaque-member-reference"),
        ])?;
        assert!(bearers.source(CallerRole::Member).is_ok());
        assert!(
            bearers
                .source(CallerRole::Admin)
                .err()
                .context("admin must be unavailable")?
                .to_string()
                .contains("ADMIN_BEARER_ENV")
        );
        Ok(())
    }

    #[test]
    fn rejects_missing_invalid_ambiguous_and_retired_inputs_without_secret_diagnostics() {
        let cases: &[&[(&str, &str)]] = &[
            &[],
            &[("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE")],
            &[("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "secret-selector!")],
            &[("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "9INVALID")],
            &[
                ("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE"),
                ("MEMBER_REFERENCE", ""),
            ],
            &[
                ("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE"),
                ("MEMBER_REFERENCE", "secret with-whitespace"),
            ],
            &[
                ("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE"),
                ("VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV", "MEMBER_REFERENCE"),
                ("MEMBER_REFERENCE", "secret-duplicate"),
            ],
            &[
                ("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE"),
                ("VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV", "ADMIN_REFERENCE"),
                ("MEMBER_REFERENCE", "secret-duplicate"),
                ("ADMIN_REFERENCE", "secret-duplicate"),
            ],
            &[
                ("VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV", "MEMBER_REFERENCE"),
                ("VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV", "ADMIN_REFERENCE"),
                ("MEMBER_REFERENCE", "opaque-member-reference"),
            ],
            &[("VEOVEO_ACCEPTANCE_KEY_DIRECTORY", "secret-key-path")],
        ];
        let expected = [
            "MEMBER_BEARER_ENV must name",
            "member bearer variable is missing",
            "member bearer selector must be an environment variable name",
            "member bearer selector must be an environment variable name",
            "member bearer variable must contain a nonempty bearer",
            "member bearer variable must contain a nonempty bearer",
            "selectors must name distinct variables",
            "bearer inputs must be distinct",
            "admin bearer variable is missing",
            "KEY_DIRECTORY is retired",
        ];
        assert_eq!(cases.len(), expected.len());
        for (entries, expected) in cases.iter().zip(expected) {
            let error = load(entries)
                .err()
                .expect("invalid input must fail")
                .to_string();
            assert!(
                !error.contains("secret"),
                "credential data reached diagnostics"
            );
            assert!(
                error.contains(expected),
                "input rejected for the wrong reason: {error}"
            );
        }
    }
}
