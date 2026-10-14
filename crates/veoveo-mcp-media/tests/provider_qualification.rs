//! Qualification against the real generation provider: one billed generation through the
//! full flow — the live catalog, the submission, completion, the output stored in
//! Artifact in process, the reconciled bill and the Task's cost.
//!
//! It is ignored by default, never part of the gates, and runs only when
//! `MEDIA_PROVIDER_QUALIFICATION=1` is set beside the run-ignored flag:
//!
//! ```text
//! MEDIA_PROVIDER_QUALIFICATION=1 cargo nextest run -p veoveo-mcp-media --all-features \
//!     --test provider_qualification --run-ignored only --no-capture
//! ```
//!
//! It reads:
//!
//! | Variable | Meaning |
//! |---|---|
//! | `MEDIA_PROVIDER_URL` | The provider's API origin |
//! | `MEDIA_PROVIDER_API_KEY` | The provider's API key; never printed |
//! | `MEDIA_OUTPUT_HOSTS` | The hosts the provider serves outputs from, comma-separated |
//! | `MEDIA_QUALIFICATION_MODEL` | The model to run; by default the text-to-image model with the lowest quote for the input among the 20 with the lowest base prices |
//! | `MEDIA_QUALIFICATION_INPUT` | The model's input as a JSON object; by default a short prompt |
//! | `MEDIA_QUALIFICATION_PUBLIC_ORIGIN` | A public origin that reaches this test, such as a temporary tunnel; it stands in for the installation's public origin, selects webhook completion and receives `{origin}/media/operations/…`. Without it the run checks the provider's status endpoint |
//! | `MEDIA_QUALIFICATION_CALLBACK_LISTEN` | Where this test listens for the forwarded webhooks, `127.0.0.1:8899` by default |
//! | `MEDIA_PROVIDER_WEBHOOK_SECRET` | The secret the provider signs webhooks with, for webhook completion |
mod support;

use std::{collections::BTreeSet, net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use secrecy::SecretString;
use serde_json::{Map, Value, json};
use support::{Provider, all_permissions, harness_for, usage_of};
use veoveo_mcp_media::{
    contract::MediaModelId,
    run::{RunCompletion, RunSettings},
};
use veoveo_task_runtime::CheckSchedule;

/// How many of the lowest base prices the qualification quotes for its input.
const QUOTED_CANDIDATES: usize = 20;

/// The text-to-image model whose quote for `input` is lowest, among the
/// [`QUOTED_CANDIDATES`] whose base prices are lowest and that admit the input. A base
/// price leaves out what the input's defaults add, so only a quote ranks the models.
async fn cheapest(
    harness: &support::Harness,
    snapshot: &veoveo_mcp_media::catalog::Snapshot,
    input: &Map<String, Value>,
) -> MediaModelId {
    let mut candidates: Vec<_> = snapshot
        .models()
        .filter(|model| model.model_type.eq_ignore_ascii_case("text-to-image"))
        .filter_map(|model| {
            model
                .base_price
                .filter(|price| *price > 0.0)
                .map(|price| (price, model.model_id.clone()))
        })
        .filter(|(_, model)| snapshot.admit_input(model, input).is_ok())
        .collect();
    candidates.sort_by(|(a, _), (b, _)| a.total_cmp(b));
    let mut cheapest: Option<(f64, MediaModelId)> = None;
    for (_, model) in candidates.into_iter().take(QUOTED_CANDIDATES) {
        let Ok(quote) = harness.provider().quote(&model, input).await else {
            continue;
        };
        if quote > 0.0 && cheapest.as_ref().is_none_or(|(best, _)| quote < *best) {
            cheapest = Some((quote, model));
        }
    }
    cheapest
        .map(|(_, model)| model)
        .expect("a quoted text-to-image model in the live catalog")
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("the qualification needs {name}"))
}

/// Forward the provider's webhooks from a fixed local port to the mesh, so a temporary
/// public tunnel to that port reaches Media's callback route.
async fn forward_webhooks(listen: SocketAddr, mesh: url::Url) {
    async fn forward(
        State(mesh): State<url::Url>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        let mut target = mesh.clone();
        target.set_path(uri.path());
        let mut request = reqwest::Client::new().post(target).body(body);
        for name in [
            "content-type",
            "webhook-id",
            "webhook-timestamp",
            "webhook-signature",
        ] {
            if let Some(value) = headers.get(name) {
                request = request.header(name, value);
            }
        }
        match request.send().await {
            Ok(response) => {
                StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            Err(_) => StatusCode::BAD_GATEWAY,
        }
    }
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .expect("the webhook forwarder's port");
    let router = Router::new()
        .route("/{*path}", post(forward))
        .with_state(mesh);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
}

#[tokio::test]
#[ignore = "bills the real provider; set MEDIA_PROVIDER_QUALIFICATION=1 to run"]
async fn one_real_generation_runs_through_the_full_flow() {
    if std::env::var("MEDIA_PROVIDER_QUALIFICATION").as_deref() != Ok("1") {
        eprintln!(
            "MEDIA_PROVIDER_QUALIFICATION is not 1; the real-provider qualification does not run"
        );
        return;
    }
    let provider_url =
        url::Url::parse(&required("MEDIA_PROVIDER_URL")).expect("MEDIA_PROVIDER_URL");
    let api_key = SecretString::from(required("MEDIA_PROVIDER_API_KEY"));
    let public_origin = std::env::var("MEDIA_QUALIFICATION_PUBLIC_ORIGIN")
        .ok()
        .map(|origin| url::Url::parse(&origin).expect("MEDIA_QUALIFICATION_PUBLIC_ORIGIN"));
    let webhook_secret = SecretString::from(
        std::env::var("MEDIA_PROVIDER_WEBHOOK_SECRET").unwrap_or_else(|_| {
            assert!(
                public_origin.is_none(),
                "webhook completion needs MEDIA_PROVIDER_WEBHOOK_SECRET"
            );
            "whsec_unused".into()
        }),
    );
    let output_hosts: BTreeSet<String> = std::env::var("MEDIA_OUTPUT_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|host| host.trim().to_ascii_lowercase())
        .filter(|host| !host.is_empty())
        .collect();
    let settings = RunSettings {
        completion: match public_origin {
            Some(_) => RunCompletion::Events,
            None => RunCompletion::status_checks(),
        },
        generation_timeout: Duration::from_secs(15 * 60),
        billing_schedule: CheckSchedule::new(
            Duration::from_secs(5),
            Duration::from_secs(10),
            1.5,
            12,
        )
        .unwrap(),
        billing_timeout: Duration::from_secs(15 * 60),
        output_max_bytes: 512 * 1024 * 1024,
        output_timeout: Duration::from_secs(10 * 60),
        output_hosts,
        verification_timeout: Duration::from_secs(10 * 60),
        output_retention: None,
    };
    let harness = harness_for(
        Provider {
            url: provider_url,
            api_key,
            webhook_secret,
            fake: None,
        },
        settings,
        public_origin.clone(),
        all_permissions(),
    )
    .await;
    if public_origin.is_some() {
        let listen: SocketAddr = std::env::var("MEDIA_QUALIFICATION_CALLBACK_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8899".into())
            .parse()
            .expect("MEDIA_QUALIFICATION_CALLBACK_LISTEN");
        forward_webhooks(listen, harness.mesh.base()).await;
    }

    // The live catalog, the input and the model to run.
    let snapshot = harness.catalog.snapshot().expect("the live catalog loaded");
    assert!(!snapshot.is_empty(), "the provider lists models");
    let input: Map<String, Value> = match std::env::var("MEDIA_QUALIFICATION_INPUT") {
        Ok(input) => {
            serde_json::from_str(&input).expect("MEDIA_QUALIFICATION_INPUT is a JSON object")
        }
        Err(_) => json!({"prompt": "A small red circle centered on a plain white background"})
            .as_object()
            .cloned()
            .unwrap(),
    };
    let model = match std::env::var("MEDIA_QUALIFICATION_MODEL") {
        Ok(model) => MediaModelId::parse(model).expect("MEDIA_QUALIFICATION_MODEL"),
        Err(_) => cheapest(&harness, &snapshot, &input).await,
    };
    if let Err(error) = snapshot.admit_input(&model, &input) {
        panic!("{model} refuses the qualification input; set MEDIA_QUALIFICATION_INPUT: {error}");
    }
    let quote = harness
        .provider()
        .quote(&model, &input)
        .await
        .expect("the provider quotes the run");
    eprintln!("qualifying {model} at a quoted price of {quote} USD");

    // One run through the full flow.
    let started = harness
        .mesh
        .start_task("media", "run", json!({"model": model, "input": input}))
        .await;
    let task = started["result"]["taskId"]
        .as_str()
        .unwrap_or_else(|| panic!("no Task started: {started}"))
        .to_owned();
    let finished = tokio::time::timeout(Duration::from_secs(30 * 60), async {
        loop {
            let view = harness.task(&task).await;
            if view["status"] != "working" {
                return view;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
    .await
    .expect("the run finished within 30 minutes");
    assert_eq!(finished["status"], "completed", "{finished}");
    let result = &finished["result"]["structuredContent"];
    let artifacts = result["artifacts"]
        .as_array()
        .expect("the result's outputs");
    assert!(
        !artifacts.is_empty(),
        "the run stored its outputs: {result}"
    );
    for artifact in artifacts {
        let bytes = harness
            .download(artifact["artifactUri"].as_str().unwrap())
            .await;
        assert!(!bytes.is_empty(), "an output holds bytes");
        assert_eq!(Some(bytes.len() as u64), artifact["size"].as_u64());
    }
    let usage = usage_of(&finished);
    assert_eq!(usage.len(), 1, "{finished}");
    assert_eq!(
        usage[0]["phase"], "actual",
        "the provider's bill was reconciled"
    );
    // The estimate, the provider's quote, covers the bill.
    let actual: f64 = usage[0]["money"]["amount"]
        .as_str()
        .and_then(|amount| amount.parse().ok())
        .expect("the actual cost's amount");
    assert!(
        actual <= quote + 1e-9,
        "the bill of {actual} USD exceeds the quote of {quote} USD"
    );
    eprintln!(
        "prediction {} stored {} output(s); quoted {quote} USD, the run cost {} {}",
        result["prediction"]["predictionId"],
        artifacts.len(),
        usage[0]["money"]["amount"],
        usage[0]["money"]["currency"]
    );
}
