//! The model catalog: an immutable snapshot of the provider's registry, replaced whole
//! when a refresh reads a valid registry. Catalog pages carry cursors bound to the
//! snapshot's digest and filters, so a cursor never pages a different registry, and a
//! run's input is checked against its model's published JSON Schema.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, RwLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use veoveo_platform_store::CursorScope;

use crate::{
    contract::{
        CatalogFilter, CatalogLimit, ListCursor, MediaModelId, ModelCatalogPage, ModelSchema,
        ModelsRequest,
    },
    provider::{ProviderClient, ProviderError, wire::ModelEntry},
};

/// How often the catalog reads the provider's registry again.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long the catalog waits before it tries again after a failed refresh.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(60);
/// The most validation errors a refused input lists.
const MAX_REPORTED_ERRORS: usize = 5;

/// Why the catalog could not answer.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("the model catalog is not loaded yet; try again shortly")]
    Unavailable,
    #[error("the provider's registry is invalid: {0}")]
    InvalidRegistry(&'static str),
    #[error(
        "the cursor belongs to another catalog page, filter or registry; start from the first page"
    )]
    StaleCursor,
    #[error("no model `{0}` is in the catalog")]
    UnknownModel(MediaModelId),
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Why a run's input was refused.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("the model's published input schema is no valid JSON Schema: {0}")]
    InvalidSchema(String),
    #[error("the input does not satisfy the model's schema: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

/// One registry, in model identity order.
#[derive(Debug)]
pub struct Snapshot {
    models: Vec<ModelEntry>,
    index: BTreeMap<MediaModelId, usize>,
    digest: String,
    pub fetched_at: DateTime<Utc>,
}

/// The filters and registry a cursor is bound to.
#[derive(Serialize)]
struct PageScope<'a> {
    query: Option<&'a str>,
    model_type: Option<&'a str>,
    limit: u32,
    registry: &'a str,
}

impl Snapshot {
    /// Admit a registry: unique model identities and finite prices.
    pub fn new(models: Vec<ModelEntry>, fetched_at: DateTime<Utc>) -> Result<Self, CatalogError> {
        let mut seen = BTreeSet::new();
        for model in &models {
            if !seen.insert(model.model_id.clone()) {
                return Err(CatalogError::InvalidRegistry("a model appears twice"));
            }
            if model.base_price.is_some_and(|price| !price.is_finite()) {
                return Err(CatalogError::InvalidRegistry(
                    "a model's price is not finite",
                ));
            }
        }
        let mut models = models;
        models.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        let bytes = serde_json::to_vec(&models)
            .map_err(|_| CatalogError::InvalidRegistry("the registry does not serialize"))?;
        let digest = hex::encode(Sha256::digest(bytes));
        let index = models
            .iter()
            .enumerate()
            .map(|(position, model)| (model.model_id.clone(), position))
            .collect();
        Ok(Self {
            models,
            index,
            digest,
            fetched_at,
        })
    }

    /// The registry's SHA-256, which binds the catalog's cursors.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Every model, in identity order.
    pub fn models(&self) -> impl Iterator<Item = &ModelEntry> {
        self.models.iter()
    }

    pub fn model(&self, model: &MediaModelId) -> Option<&ModelEntry> {
        self.index
            .get(model)
            .map(|position| &self.models[*position])
    }

    /// One model and its input schema.
    pub fn schema(&self, model: &MediaModelId) -> Result<ModelSchema, CatalogError> {
        self.model(model)
            .map(ModelEntry::schema)
            .ok_or_else(|| CatalogError::UnknownModel(model.clone()))
    }

    fn scope(
        &self,
        query: Option<&CatalogFilter>,
        model_type: Option<&CatalogFilter>,
        limit: CatalogLimit,
    ) -> Result<CursorScope, CatalogError> {
        CursorScope::new(
            "media.models",
            "catalog",
            &PageScope {
                query: query.map(CatalogFilter::as_str),
                model_type: model_type.map(CatalogFilter::as_str),
                limit: limit.get(),
                registry: &self.digest,
            },
        )
        .map_err(|_| CatalogError::StaleCursor)
    }

    /// One page of models matching the request's filters, after the request's cursor.
    pub fn page(&self, request: &ModelsRequest) -> Result<ModelCatalogPage, CatalogError> {
        let limit = request.limit.unwrap_or_default();
        let scope = self.scope(request.query.as_ref(), request.model_type.as_ref(), limit)?;
        let after: Option<MediaModelId> = request
            .cursor
            .as_ref()
            .map(|cursor| {
                scope
                    .parse::<MediaModelId>(cursor.as_str())
                    .map(|cursor| cursor.position().clone())
                    .map_err(|_| CatalogError::StaleCursor)
            })
            .transpose()?;
        let matches = |model: &&ModelEntry| {
            request
                .model_type
                .as_ref()
                .is_none_or(|wanted| model.model_type.eq_ignore_ascii_case(wanted.as_str()))
                && request.query.as_ref().is_none_or(|query| {
                    [
                        model.model_id.as_str(),
                        &model.name,
                        &model.model_type,
                        &model.description,
                    ]
                    .iter()
                    .any(|text| text.to_ascii_lowercase().contains(query.as_str()))
                })
        };
        let matching: Vec<&ModelEntry> = self.models.iter().filter(matches).collect();
        if let Some(after) = &after
            && !matching.iter().any(|model| &model.model_id == after)
        {
            return Err(CatalogError::StaleCursor);
        }
        let mut remaining = matching
            .iter()
            .filter(|model| after.as_ref().is_none_or(|after| &model.model_id > after));
        let models: Vec<_> = remaining
            .by_ref()
            .take(limit.get() as usize)
            .map(|model| model.catalog_item())
            .collect();
        let next_cursor = match (remaining.next(), models.last()) {
            (Some(_), Some(last)) => Some(
                ListCursor::parse(
                    scope
                        .cursor(last.model_id.clone())
                        .map_err(|_| CatalogError::StaleCursor)?
                        .as_str(),
                )
                .map_err(|_| CatalogError::StaleCursor)?,
            ),
            _ => None,
        };
        Ok(ModelCatalogPage {
            query: request.query.clone(),
            model_type: request.model_type.clone(),
            total_available: matching.len() as u64,
            limit,
            models,
            next_cursor,
        })
    }

    /// The model, once `input` satisfies its published input schema. A model that
    /// publishes no schema takes any object, which the provider checks itself.
    pub fn admit_input(
        &self,
        model: &MediaModelId,
        input: &Map<String, Value>,
    ) -> Result<&ModelEntry, InputError> {
        let entry = self
            .model(model)
            .ok_or_else(|| CatalogError::UnknownModel(model.clone()))?;
        let Some(schema) = entry.request_schema() else {
            return Ok(entry);
        };
        let validator = jsonschema::validator_for(schema)
            .map_err(|error| InputError::InvalidSchema(error.to_string()))?;
        let instance = Value::Object(input.clone());
        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .take(MAX_REPORTED_ERRORS)
            .map(|error| {
                let path = error.instance_path().to_string();
                if path.is_empty() {
                    error.to_string()
                } else {
                    format!("{path}: {error}")
                }
            })
            .collect();
        if errors.is_empty() {
            Ok(entry)
        } else {
            Err(InputError::Invalid(errors))
        }
    }
}

/// The current snapshot and the provider it is read from.
pub struct Catalog {
    provider: ProviderClient,
    current: RwLock<Option<Arc<Snapshot>>>,
}

impl Catalog {
    pub fn new(provider: ProviderClient) -> Self {
        Self {
            provider,
            current: RwLock::new(None),
        }
    }

    /// The current snapshot, once one is loaded.
    pub fn snapshot(&self) -> Result<Arc<Snapshot>, CatalogError> {
        self.current
            .read()
            .expect("catalog lock")
            .clone()
            .ok_or(CatalogError::Unavailable)
    }

    /// Replace the snapshot with `models`, once they form a valid registry.
    pub fn install(&self, models: Vec<ModelEntry>) -> Result<Arc<Snapshot>, CatalogError> {
        let snapshot = Arc::new(Snapshot::new(models, Utc::now())?);
        *self.current.write().expect("catalog lock") = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Read the provider's registry and install it. A failed read keeps the current
    /// snapshot.
    pub async fn refresh(&self) -> Result<Arc<Snapshot>, CatalogError> {
        let models = self.provider.models().await?;
        self.install(models)
    }

    /// Refresh at once and then every hour until `stop`, trying again a minute after a
    /// failure.
    pub async fn keep_fresh(self: Arc<Self>, stop: CancellationToken) {
        loop {
            let wait = match self.refresh().await {
                Ok(snapshot) => {
                    tracing::info!(models = snapshot.len(), "refreshed the model catalog");
                    REFRESH_INTERVAL
                }
                Err(error) => {
                    tracing::warn!(%error, "the model catalog could not be refreshed");
                    RETRY_INTERVAL
                }
            };
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, model_type: &str, schema: Option<Value>) -> ModelEntry {
        ModelEntry {
            model_id: MediaModelId::parse(id).unwrap(),
            name: id.rsplit('/').next().unwrap().into(),
            model_type: model_type.into(),
            description: format!("{model_type} model"),
            base_price: Some(0.01),
            formula: None,
            api_schema: schema.map(|request_schema| {
                json!({"api_schemas": [{"type": "model_run", "request_schema": request_schema}]})
            }),
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot::new(
            vec![
                entry("b/image", "text-to-image", None),
                entry("a/video", "text-to-video", None),
                entry(
                    "c/image",
                    "text-to-image",
                    Some(json!({
                        "type": "object",
                        "required": ["prompt"],
                        "properties": {"prompt": {"type": "string", "minLength": 1}},
                        "additionalProperties": false
                    })),
                ),
                entry("d/edit", "image-to-image", None),
            ],
            Utc::now(),
        )
        .unwrap()
    }

    fn request(model_type: Option<&str>, limit: u32, cursor: Option<ListCursor>) -> ModelsRequest {
        ModelsRequest {
            query: None,
            model_type: model_type.map(|t| CatalogFilter::parse(t).unwrap().unwrap()),
            limit: Some(CatalogLimit::new(limit).unwrap()),
            cursor,
        }
    }

    #[test]
    fn pages_follow_model_order_and_end_without_a_cursor() {
        let snapshot = snapshot();
        let first = snapshot.page(&request(None, 3, None)).unwrap();
        assert_eq!(first.total_available, 4);
        let ids: Vec<_> = first.models.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(ids, ["a/video", "b/image", "c/image"]);
        let second = snapshot
            .page(&request(None, 3, first.next_cursor.clone()))
            .unwrap();
        assert_eq!(second.models.len(), 1);
        assert_eq!(second.models[0].model_id.as_str(), "d/edit");
        assert_eq!(second.next_cursor, None);
    }

    #[test]
    fn filters_select_by_type_and_query() {
        let snapshot = snapshot();
        let images = snapshot
            .page(&request(Some("TEXT-to-image"), 10, None))
            .unwrap();
        assert_eq!(images.total_available, 2);
        let query = ModelsRequest {
            query: CatalogFilter::parse("EDIT").unwrap(),
            ..ModelsRequest::default()
        };
        let edits = snapshot.page(&query).unwrap();
        assert_eq!(edits.models.len(), 1);
        assert_eq!(edits.models[0].model_id.as_str(), "d/edit");
    }

    #[test]
    fn a_cursor_pages_only_its_own_filter_limit_and_registry() {
        let snapshot = snapshot();
        let first = snapshot.page(&request(None, 1, None)).unwrap();
        let cursor = first.next_cursor.unwrap();
        assert!(matches!(
            snapshot.page(&request(None, 2, Some(cursor.clone()))),
            Err(CatalogError::StaleCursor)
        ));
        assert!(matches!(
            snapshot.page(&request(Some("text-to-image"), 1, Some(cursor.clone()))),
            Err(CatalogError::StaleCursor)
        ));
        let other = Snapshot::new(
            vec![
                entry("a/video", "text-to-video", None),
                entry("z/new", "text-to-image", None),
            ],
            Utc::now(),
        )
        .unwrap();
        assert!(matches!(
            other.page(&request(None, 1, Some(cursor))),
            Err(CatalogError::StaleCursor)
        ));
    }

    #[test]
    fn a_registry_with_duplicates_or_bad_prices_is_refused() {
        assert!(
            Snapshot::new(
                vec![entry("a/x", "t", None), entry("a/x", "t", None)],
                Utc::now()
            )
            .is_err()
        );
        let mut priced = entry("a/x", "t", None);
        priced.base_price = Some(f64::INFINITY);
        assert!(Snapshot::new(vec![priced], Utc::now()).is_err());
    }

    #[test]
    fn inputs_are_checked_against_the_published_schema() {
        let snapshot = snapshot();
        let model = MediaModelId::parse("c/image").unwrap();
        let good = json!({"prompt": "a lighthouse"})
            .as_object()
            .unwrap()
            .clone();
        assert!(snapshot.admit_input(&model, &good).is_ok());
        let bad = json!({"prompt": "", "extra": 1})
            .as_object()
            .unwrap()
            .clone();
        match snapshot.admit_input(&model, &bad) {
            Err(InputError::Invalid(errors)) => assert!(!errors.is_empty()),
            other => panic!("expected an invalid input, got {other:?}"),
        }
        // Without a published schema the provider checks the input.
        let open = MediaModelId::parse("a/video").unwrap();
        assert!(snapshot.admit_input(&open, &bad).is_ok());
        assert!(matches!(
            snapshot.admit_input(&MediaModelId::parse("x/none").unwrap(), &good),
            Err(InputError::Catalog(CatalogError::UnknownModel(_)))
        ));
    }
}
