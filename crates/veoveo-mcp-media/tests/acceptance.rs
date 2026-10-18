//! Acceptance through a running local installation: Media registered with the Hub,
//! called through the Hub by the installation's member and admin callers, with each
//! output stored in Artifact for the caller and its bytes read from the installation's
//! object store through the URL Artifact signs.
//!
//! Live cases are ignored by default. Use the native HTTPS `VEOVEO_ACCEPTANCE_HUB`
//! and prepared `VEOVEO_ACCEPTANCE_{MEMBER,ADMIN}_BEARER_ENV` selectors, whose
//! distinct variables hold bearer/opaque references. No caller signing keys are
//! copied. The two fake-provider cases need the testing feature and Media configured
//! for the private fake. They do not qualify deployed cancellation or restart recovery;
//! see the README and the installation's execution checklist.
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
use veoveo_hub_sdk::client::{BearerSource as _, StaticBearer};
use veoveo_mcp_artifact::contract::{CreateDownloadResult, SignedTransfer, TransferMethod};
use veoveo_mcp_media::{
    contract::{CompletionMode, GenerationResult, ModelCatalogPage, PredictionView},
    fake::{IMAGE_BYTES, IMAGE_MODEL, output_byte},
};

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
        let http = reqwest::Client::builder().build()?;
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

    async fn connect(&self, caller: CallerRole) -> Result<Client> {
        let token = self.token(caller).await?;
        let transport = StreamableHttpClientTransport::with_client(
            self.http.clone(),
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
#[ignore = "requires a running local installation with the fake provider"]
async fn a_run_through_the_hub_stores_its_output_in_artifact_with_its_cost() -> Result<()> {
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
    let download: CreateDownloadResult = call(
        &admin,
        "artifact",
        "create_download",
        json!({"artifactId": artifact_id}),
    )
    .await?;
    let bytes = installation.download(&download.get).await?;
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
