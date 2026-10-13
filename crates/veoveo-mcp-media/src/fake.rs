//! The fake provider: a stand-in for the generation provider that speaks its wire format,
//! so tests and the local installation never reach the real one. It serves a small
//! registry, accepts submissions, finishes each after a delay and posts the outcome to the
//! submission's webhook signed with the webhook secret, or keeps it at the prediction's
//! status endpoint when the submission named no webhook. It serves the outputs, answers
//! billing searches and deletes predictions on cancellation. Tests can hold outcomes back
//! and send events by hand.
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use chrono::Utc;
use futures::stream;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use url::Url;

use crate::{
    contract::{MediaModelId, MediaPredictionId},
    provider::{
        webhook::{ID_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER, sign},
        wire::{
            BillingQuery, BillingRecord, BillingSearch, CancellationReceipt, CancellationRequest,
            Envelope, ModelEntry, Prediction,
        },
    },
};

/// The fake's image model: a prompt makes one small PNG-typed output.
pub const IMAGE_MODEL: &str = "fake/text-to-image";
/// The fake's large model: one output larger than one Artifact part.
pub const LARGE_MODEL: &str = "fake/large-video";
/// The fake's failing model: every run fails at the provider.
pub const FAILING_MODEL: &str = "fake/always-fails";
/// What one run of any fake model costs, in US dollars.
pub const PRICE_USD: f64 = 0.04;
/// The bytes of an image model's output.
pub const IMAGE_BYTES: u64 = 64 * 1024;
/// The bytes of the large model's output.
pub const LARGE_BYTES: u64 = 20 * 1024 * 1024 + 123;

/// The byte at `offset` of every fake output, so a test can check any range.
pub fn output_byte(offset: u64) -> u8 {
    (offset.wrapping_mul(0x9E37_79B1) >> 11) as u8
}

/// The fake's registry.
pub fn registry() -> Vec<ModelEntry> {
    let schema = |required: &[&str]| {
        json!({"api_schemas": [{"type": "model_run", "request_schema": {
            "type": "object",
            "required": required,
            "properties": {
                "prompt": {"type": "string", "minLength": 1},
                "image_url": {"type": "string", "format": "uri"}
            },
            "additionalProperties": false
        }}]})
    };
    let model = |id: &str, model_type: &str, description: &str, api_schema: Value| ModelEntry {
        model_id: MediaModelId::parse(id).expect("a fake model identity"),
        name: id.rsplit('/').next().unwrap_or(id).to_owned(),
        model_type: model_type.to_owned(),
        description: description.to_owned(),
        base_price: Some(PRICE_USD),
        formula: Some("per run".to_owned()),
        api_schema: Some(api_schema),
    };
    vec![
        model(
            IMAGE_MODEL,
            "text-to-image",
            "Makes one image from a prompt.",
            schema(&["prompt"]),
        ),
        model(
            LARGE_MODEL,
            "text-to-video",
            "Makes one large video from a prompt.",
            schema(&["prompt"]),
        ),
        model(
            FAILING_MODEL,
            "text-to-image",
            "Fails every run.",
            schema(&["prompt"]),
        ),
    ]
}

/// One prediction the fake holds.
#[derive(Debug, Clone)]
struct Held {
    prediction: Prediction,
    webhook: Option<Url>,
    billed: bool,
}

struct FakeState {
    base: Url,
    secret: String,
    deliver: AtomicBool,
    delay: Duration,
    next: AtomicU64,
    events: AtomicU64,
    submissions: AtomicU64,
    cancellations: AtomicU64,
    status_reads: AtomicU64,
    predictions: Mutex<BTreeMap<String, Held>>,
    http: reqwest::Client,
}

/// A running fake provider.
#[derive(Clone)]
pub struct FakeProvider {
    state: Arc<FakeState>,
}

/// How a fake provider starts.
#[derive(Debug, Clone)]
pub struct FakeProviderConfig {
    pub listen: SocketAddr,
    /// The webhook secret, `whsec_` and the key.
    pub webhook_secret: String,
    /// How long a prediction runs before the fake posts its outcome.
    pub delay: Duration,
    /// Whether outcomes are posted on their own. A test that holds them back sends them
    /// with [`FakeProvider::complete`].
    pub deliver: bool,
}

impl FakeProvider {
    /// Start the fake on `config.listen` and serve until the process ends.
    pub async fn start(config: FakeProviderConfig) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(config.listen).await?;
        let address = listener.local_addr()?;
        let base = Url::parse(&format!("http://{address}")).expect("a loopback URL");
        let state = Arc::new(FakeState {
            base,
            secret: config.webhook_secret,
            deliver: AtomicBool::new(config.deliver),
            delay: config.delay,
            next: AtomicU64::new(1),
            events: AtomicU64::new(1),
            submissions: AtomicU64::new(0),
            cancellations: AtomicU64::new(0),
            status_reads: AtomicU64::new(0),
            predictions: Mutex::new(BTreeMap::new()),
            http: reqwest::Client::new(),
        });
        let router = Router::new()
            .route("/api/v3/models", get(models))
            .route("/api/v3/billings/search", post(billing))
            .route("/api/v3/predictions/delete", post(cancel))
            .route("/api/v3/predictions/{prediction}/result", get(status))
            .route("/api/v3/{*model}", post(submit))
            .route("/outputs/{prediction}/{file}", get(output))
            .with_state(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self { state })
    }

    /// The fake's API origin, which Media's provider address names.
    pub fn base(&self) -> &Url {
        &self.state.base
    }

    /// How many submissions the fake accepted.
    pub fn submissions(&self) -> u64 {
        self.state.submissions.load(Ordering::SeqCst)
    }

    /// How many cancellations the fake received.
    pub fn cancellations(&self) -> u64 {
        self.state.cancellations.load(Ordering::SeqCst)
    }

    /// Whether outcomes are posted on their own from now on.
    pub fn deliver_outcomes(&self, deliver: bool) {
        self.state.deliver.store(deliver, Ordering::SeqCst);
    }

    /// The predictions the fake holds, oldest first.
    pub fn predictions(&self) -> Vec<MediaPredictionId> {
        let held = self
            .state
            .predictions
            .lock()
            .expect("fake predictions lock");
        let mut ids: Vec<_> = held.values().map(|held| held.prediction.clone()).collect();
        ids.sort_by_key(|prediction| prediction.created_at);
        ids.into_iter().map(|prediction| prediction.id).collect()
    }

    /// Finish `prediction` without posting its outcome, as for a submission that named no
    /// webhook: its status endpoint reports it.
    pub fn finish(&self, prediction: &MediaPredictionId) -> bool {
        finish(&self.state, prediction.as_str()).is_some()
    }

    /// How many status reads the fake answered.
    pub fn status_reads(&self) -> u64 {
        self.state.status_reads.load(Ordering::SeqCst)
    }

    /// Finish `prediction` and post its outcome now, under a fresh event identity.
    pub async fn complete(&self, prediction: &MediaPredictionId) -> Result<StatusCode, String> {
        let event = finish(&self.state, prediction.as_str()).ok_or("no such prediction")?;
        let event_id = self.next_event();
        self.post(&event, &event_id, true).await
    }

    /// Post the prediction's current state under `event_id`, signed or forged.
    pub async fn post_event(
        &self,
        prediction: &MediaPredictionId,
        event_id: &str,
        signed: bool,
    ) -> Result<StatusCode, String> {
        let held = self
            .state
            .predictions
            .lock()
            .expect("fake predictions lock")
            .get(prediction.as_str())
            .cloned()
            .ok_or("no such prediction")?;
        self.post(&held, event_id, signed).await
    }

    fn next_event(&self) -> String {
        format!("evt_{}", self.state.events.fetch_add(1, Ordering::SeqCst))
    }

    async fn post(&self, held: &Held, event_id: &str, signed: bool) -> Result<StatusCode, String> {
        post_webhook(&self.state, held, event_id, signed).await
    }
}

/// Mark a prediction finished: completed with its output, or failed for the failing
/// model. Returns its state for the webhook.
fn finish(state: &FakeState, prediction: &str) -> Option<Held> {
    let mut held = state.predictions.lock().expect("fake predictions lock");
    let entry = held.get_mut(prediction)?;
    if entry.prediction.status == "processing" {
        if entry.prediction.model.as_str() == FAILING_MODEL {
            entry.prediction.status = "failed".into();
            entry.prediction.error = Some("the fake model always fails".into());
        } else {
            entry.prediction.status = "completed".into();
            let extension = if entry.prediction.model.as_str() == LARGE_MODEL {
                "mp4"
            } else {
                "png"
            };
            let mut url = state.base.clone();
            url.path_segments_mut().expect("an http URL").extend([
                "outputs",
                prediction,
                &format!("output-0.{extension}"),
            ]);
            entry.prediction.outputs = vec![url.to_string()];
            entry.prediction.execution_time = Some(1.5);
        }
        entry.billed = true;
    }
    Some(entry.clone())
}

async fn post_webhook(
    state: &FakeState,
    held: &Held,
    event_id: &str,
    signed: bool,
) -> Result<StatusCode, String> {
    let webhook = held
        .webhook
        .clone()
        .ok_or("the submission named no webhook")?;
    let body = serde_json::to_vec(&held.prediction).map_err(|error| error.to_string())?;
    let timestamp = Utc::now().timestamp().to_string();
    let signature = if signed {
        sign(&state.secret, event_id, &timestamp, &body)
    } else {
        sign("whsec_forged", event_id, &timestamp, &body)
    };
    let response = state
        .http
        .post(webhook)
        .header(header::CONTENT_TYPE, "application/json")
        .header(ID_HEADER, event_id)
        .header(TIMESTAMP_HEADER, timestamp)
        .header(SIGNATURE_HEADER, signature)
        .body(body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| error.without_url().to_string())?;
    Ok(response.status())
}

async fn models(State(_): State<Arc<FakeState>>) -> Json<Envelope<Vec<ModelEntry>>> {
    Json(Envelope::ok(registry()))
}

#[derive(Deserialize)]
struct SubmitQuery {
    webhook: Option<Url>,
}

async fn submit(
    State(state): State<Arc<FakeState>>,
    Path(model): Path<String>,
    Query(query): Query<SubmitQuery>,
    Json(input): Json<Map<String, Value>>,
) -> Response {
    let Ok(model) = MediaModelId::parse(model) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !registry().iter().any(|entry| entry.model_id == model) {
        return Json(Envelope {
            code: 404,
            message: "unknown model".into(),
            data: Value::Null,
        })
        .into_response();
    }
    if input
        .get("prompt")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Json(Envelope {
            code: 400,
            message: "a prompt is required".into(),
            data: Value::Null,
        })
        .into_response();
    }
    state.submissions.fetch_add(1, Ordering::SeqCst);
    let id = format!("fake-{}", state.next.fetch_add(1, Ordering::SeqCst));
    let mut status_url = state.base.clone();
    status_url
        .path_segments_mut()
        .expect("an http URL")
        .extend(["api", "v3", "predictions", &id, "result"]);
    let prediction = Prediction {
        id: MediaPredictionId::parse(id.clone()).expect("a fake prediction identity"),
        model,
        outputs: Vec::new(),
        urls: Some(crate::provider::wire::PredictionUrls {
            get: status_url.to_string(),
        }),
        status: "processing".into(),
        created_at: Some(Utc::now()),
        error: None,
        execution_time: None,
    };
    state
        .predictions
        .lock()
        .expect("fake predictions lock")
        .insert(
            id.clone(),
            Held {
                prediction: prediction.clone(),
                webhook: query.webhook,
                billed: false,
            },
        );
    if state.deliver.load(Ordering::SeqCst) {
        let state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(state.delay).await;
            if !state.deliver.load(Ordering::SeqCst) {
                return;
            }
            if let Some(held) = finish(&state, &id)
                && held.webhook.is_some()
            {
                let event_id = format!("evt_{}", state.events.fetch_add(1, Ordering::SeqCst));
                if let Err(error) = post_webhook(&state, &held, &event_id, true).await {
                    eprintln!("the fake provider could not post its webhook: {error}");
                }
            }
        });
    }
    Json(Envelope::ok(prediction)).into_response()
}

async fn billing(
    State(state): State<Arc<FakeState>>,
    Json(query): Json<BillingQuery>,
) -> Json<Envelope<BillingSearch>> {
    let held = state.predictions.lock().expect("fake predictions lock");
    let items = query
        .prediction_uuids
        .iter()
        .filter_map(|prediction| held.get(prediction.as_str()))
        .filter(|held| held.billed)
        .map(|held| BillingRecord {
            uuid: format!("bill-{}", held.prediction.id),
            billing_type: "deduct".into(),
            price: Some(PRICE_USD),
            created_at: Some(Utc::now()),
        })
        .collect();
    Json(Envelope::ok(BillingSearch { items }))
}

async fn cancel(
    State(state): State<Arc<FakeState>>,
    Json(request): Json<CancellationRequest>,
) -> Json<Envelope<CancellationReceipt>> {
    state.cancellations.fetch_add(1, Ordering::SeqCst);
    let mut held = state.predictions.lock().expect("fake predictions lock");
    let mut deleted = 0;
    for prediction in &request.ids {
        if let Some(entry) = held.get_mut(prediction.as_str())
            && entry.prediction.status == "processing"
        {
            entry.prediction.status = "cancelled".into();
            deleted += 1;
        }
    }
    Json(Envelope::ok(CancellationReceipt {
        deleted_count: deleted,
    }))
}

async fn status(State(state): State<Arc<FakeState>>, Path(prediction): Path<String>) -> Response {
    state.status_reads.fetch_add(1, Ordering::SeqCst);
    let held = state.predictions.lock().expect("fake predictions lock");
    match held.get(&prediction) {
        Some(entry) => Json(Envelope::ok(entry.prediction.clone())).into_response(),
        None => Json(Envelope {
            code: 404,
            message: "unknown prediction".into(),
            data: Value::Null,
        })
        .into_response(),
    }
}

async fn output(
    State(state): State<Arc<FakeState>>,
    Path((prediction, file)): Path<(String, String)>,
) -> Response {
    let model = {
        let held = state.predictions.lock().expect("fake predictions lock");
        let Some(entry) = held.get(&prediction) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if entry.prediction.status != "completed" || !file.starts_with("output-0.") {
            return StatusCode::NOT_FOUND.into_response();
        }
        entry.prediction.model.clone()
    };
    let (length, media_type) = if model.as_str() == LARGE_MODEL {
        (LARGE_BYTES, "video/mp4")
    } else {
        (IMAGE_BYTES, "image/png")
    };
    const CHUNK: u64 = 256 * 1024;
    let chunks = stream::iter((0..length.div_ceil(CHUNK)).map(move |index| {
        let start = index * CHUNK;
        let end = (start + CHUNK).min(length);
        Ok::<_, std::io::Error>(Bytes::from(
            (start..end).map(output_byte).collect::<Vec<_>>(),
        ))
    }));
    let mut response = Response::new(Body::from_stream(chunks));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}
