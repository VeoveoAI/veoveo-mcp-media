//! Acceptance through a running local installation: Media registered with the Gateway,
//! called through the Gateway by the installation's member and admin callers, with each
//! output stored in Artifact for the caller and its bytes read from the installation's
//! object store through the URL Artifact signs.
//!
//! The tests need a running installation, so they are ignored by default. Inside the
//! Media environment, set `VEOVEO_ACCEPTANCE_GATEWAY` to the Gateway origin and
//! `VEOVEO_ACCEPTANCE_KEY_DIRECTORY` to the directory holding the callers' keys, then
//! run `cargo nextest run --test acceptance --run-ignored only`. The fake-provider cases
//! need Media running against the fake provider.
//!
//! One case bills the real provider once and runs only with
//! `MEDIA_PROVIDER_QUALIFICATION=1`, against Media configured for the real provider:
//!
//! | Variable | Meaning |
//! |---|---|
//! | `MEDIA_QUALIFICATION_MODEL` | The model to run, such as the one `tests/provider_qualification.rs` quotes lowest |
//! | `MEDIA_QUALIFICATION_INPUT` | The model's input as a JSON object; by default a short prompt |
//! | `MEDIA_QUALIFICATION_COMPLETION` | How the installation's Media learns outcomes: `events` with a public origin, `checks` without |
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow, bail, ensure};
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{
        CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig, GetTaskParams,
        Implementation, ProtocolVersion, ReadResourceRequestParams, ResourceContents, TaskPayload,
        TaskStatus,
    },
    service::{RunningService, ServiceError},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use veoveo_mcp_media::{
    contract::{CompletionMode, GenerationResult, ModelCatalogPage, PredictionView},
    fake::{IMAGE_BYTES, IMAGE_MODEL, output_byte},
};
use veoveo_oauth_client::{ClientAssertionKeyId, ClientAssertionSigner};
use veoveo_types::OAuthClientId;

const MEMBER: &str = "acceptance-member";
const ADMIN: &str = "acceptance-admin";

struct Caller;

impl ClientHandler for Caller {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::builder().enable_tasks().build(),
            Implementation::new("veoveo-mcp-media-acceptance", env!("CARGO_PKG_VERSION")),
        )
    }
}

type Client = RunningService<RoleClient, Caller>;

struct Installation {
    gateway: url::Url,
    keys: PathBuf,
}

impl Installation {
    fn from_env() -> Result<Self> {
        // reqwest is built without a TLS provider; Media uses rustls with ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let gateway = std::env::var("VEOVEO_ACCEPTANCE_GATEWAY")
            .context("VEOVEO_ACCEPTANCE_GATEWAY names the Gateway origin")?;
        let keys = std::env::var("VEOVEO_ACCEPTANCE_KEY_DIRECTORY")
            .context("VEOVEO_ACCEPTANCE_KEY_DIRECTORY names the callers' key directory")?;
        Ok(Self {
            gateway: url::Url::parse(&gateway)?,
            keys: PathBuf::from(keys),
        })
    }

    fn mcp(&self) -> Result<url::Url> {
        Ok(self.gateway.join("/mcp/local")?)
    }

    /// An RFC 6749 client-credentials token, authenticated with an RFC 7523 assertion.
    async fn token(&self, caller: &str) -> Result<String> {
        let jwks: Value = serde_json::from_slice(&std::fs::read(
            self.keys.join(format!("{caller}.jwks.json")),
        )?)?;
        let kid = jwks["keys"][0]["kid"]
            .as_str()
            .context("caller key identifier")?;
        let pem = std::fs::read(self.keys.join(format!("{caller}.pem")))?;
        let signer = ClientAssertionSigner::from_rsa_pem(ClientAssertionKeyId::parse(kid)?, &pem)?;
        let endpoint = self.gateway.join("/oauth/token")?;
        let assertion = signer.assertion(&OAuthClientId::parse(caller)?, &endpoint)?;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair("client_id", caller)
            .append_pair(
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            )
            .append_pair("client_assertion", assertion.expose_secret())
            .append_pair("resource", self.mcp()?.as_str())
            .append_pair("scope", "mcp")
            .finish();
        let response = reqwest::Client::new()
            .post(endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await?;
        let status = response.status();
        ensure!(status.is_success(), "token endpoint answered {status}");
        let token: Value = response.json().await?;
        Ok(token["access_token"]
            .as_str()
            .context("access_token")?
            .to_owned())
    }

    /// Media's generation usage in `caller`'s own daily cost view at the Gateway, from
    /// yesterday through tomorrow.
    async fn generation_usage(&self, caller: &str) -> Result<GenerationUsage> {
        let token = self.token(caller).await?;
        let today = chrono::Utc::now().date_naive();
        let mut totals = GenerationUsage::default();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = self.gateway.join("/admin/local/usage/daily")?;
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
            let response = reqwest::Client::new()
                .get(url)
                .bearer_auth(&token)
                .send()
                .await?;
            let status = response.status();
            ensure!(status.is_success(), "the cost view answered {status}");
            let page: Value = response.json().await?;
            for row in page["rows"].as_array().context("the cost view's rows")? {
                if row["server"] != "media" || row["kind"] != "media.generation" {
                    continue;
                }
                let records = row["records"].as_u64().unwrap_or(0);
                match row["phase"].as_str() {
                    Some("estimate") => totals.estimates += records,
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

    async fn connect(&self, caller: &str) -> Result<Client> {
        let token = self.token(caller).await?;
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(self.mcp()?.to_string())
                .auth_header(token),
        );
        Ok(Caller
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
        "{name} returned an error: {:?}",
        result.content
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
                status => bail!("the run reported {status:?}: {task:?}"),
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
#[ignore = "requires a running local installation with the fake provider"]
async fn a_run_through_the_gateway_stores_its_output_in_artifact_with_its_cost() -> Result<()> {
    let installation = Installation::from_env()?;
    let admin = installation.connect(ADMIN).await?;
    let models: ModelCatalogPage = call(&admin, "media", "models", json!({})).await?;
    ensure!(
        models
            .models
            .iter()
            .any(|model| model.model_id.as_str() == IMAGE_MODEL),
        "the catalog lists the fake image model"
    );
    let (task_id, finished) = generate(
        &admin,
        IMAGE_MODEL,
        json!({"prompt": "a lighthouse at dusk"}),
        Duration::from_secs(120),
    )
    .await?;
    let result: GenerationResult = serde_json::from_value(finished["result"].clone())?;
    ensure!(result.artifacts.len() == 1, "the run stored one output");
    let output = &result.artifacts[0];
    ensure!(
        output.size == Some(IMAGE_BYTES),
        "the output keeps its size"
    );

    // The output is the caller's in Artifact: they download it through Artifact.
    let artifact_id = output
        .artifact_uri
        .as_str()
        .strip_prefix("artifact://artifacts/")
        .context("an Artifact address")?;
    let download: Value = call(
        &admin,
        "artifact",
        "create_download",
        json!({"artifactId": artifact_id}),
    )
    .await?;
    let url = download["get"]["url"].as_str().context("a signed GET")?;
    let bytes = reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .bytes()
        .await?;
    ensure!(
        bytes
            .iter()
            .enumerate()
            .all(|(offset, byte)| *byte == output_byte(offset as u64))
            && bytes.len() as u64 == IMAGE_BYTES,
        "the stored bytes are the provider's"
    );

    // The prediction is the caller's, and the Task carries the run's cost.
    let prediction = result
        .prediction
        .prediction_id
        .clone()
        .context("the provider's prediction")?;
    let view: PredictionView = read(&admin, &format!("media://prediction/{prediction}")).await?;
    ensure!(view.result_uri.is_some(), "a succeeded run has a result");
    let usage = finished["view"].to_string();
    ensure!(
        usage.contains("ai.veoveo/usage") && usage.contains("media.generation"),
        "the Task carries its usage: {usage}"
    );
    eprintln!(
        "run {task_id}: stored {} bytes as {}",
        IMAGE_BYTES, output.artifact_uri
    );
    Ok(())
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
        Err(ServiceError::McpError(_)) => Ok(()),
        Ok(CallToolResponse::Complete(result)) if result.is_error.unwrap_or(false) => Ok(()),
        Ok(other) => bail!("run was served to the member: {other:?}"),
        Err(error) => Err(error).context("run failed without a refusal"),
    }
}

#[tokio::test]
#[ignore = "bills the real provider; set MEDIA_PROVIDER_QUALIFICATION=1 with Media configured for it"]
async fn a_real_provider_run_through_the_gateway_stores_its_output_and_records_its_costs()
-> Result<()> {
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
        let download: Value = call(
            &admin,
            "artifact",
            "create_download",
            json!({"artifactId": artifact_id}),
        )
        .await?;
        let url = download["get"]["url"].as_str().context("a signed GET")?;
        let bytes = reqwest::Client::new()
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
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
