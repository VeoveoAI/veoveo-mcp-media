//! Acceptance through a running local installation: Media registered with the Gateway
//! and running against the fake provider, called through the Gateway by the
//! installation's member and admin callers, with each output stored in Artifact for the
//! caller and its bytes read from the installation's object store through the URL
//! Artifact signs.
//!
//! The tests need a running installation, so they are ignored by default. Inside the
//! Media environment, set `VEOVEO_ACCEPTANCE_GATEWAY` to the Gateway origin and
//! `VEOVEO_ACCEPTANCE_KEY_DIRECTORY` to the directory holding the callers' keys, then
//! run `cargo nextest run --test acceptance --run-ignored only`. Media must run against
//! the fake provider; nothing here reaches a real one.
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail, ensure};
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
    contract::{GenerationResult, ModelCatalogPage, PredictionView},
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

/// Start `run` as a Task and wait up to two minutes for it to finish.
async fn generate(client: &Client, model: &str) -> Result<(String, Value)> {
    let request = tool(
        "media",
        "run",
        json!({"model": model, "input": {"prompt": "a lighthouse at dusk"}}),
    )?;
    let created = match client.call_tool_once(request).await? {
        CallToolResponse::Task(created) => created.task,
        other => bail!("run did not create a Task: {other:?}"),
    };
    let task_id = created.task_id.clone();
    let finished = tokio::time::timeout(Duration::from_secs(120), async {
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
    .context("the run did not finish within 120 seconds")??;
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
    let (task_id, finished) = generate(&admin, IMAGE_MODEL).await?;
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
