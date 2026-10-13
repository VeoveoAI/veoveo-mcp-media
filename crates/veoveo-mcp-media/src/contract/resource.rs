//! Media's resource addresses under the `media` scheme. The route declaration owns each
//! shape and its typed parameters; the shared URI machinery owns encoding and parsing, so
//! no address is built or split by hand.
use std::{borrow::Cow, error::Error, fmt};

use serde::{Deserialize, Serialize};
use veoveo_types::{
    ResourceFieldCodec, ResourceRouteError, ResourceTailCodec, ResourceUriError, Vocabulary,
};

use super::{ListCursor, MediaModelId, MediaPredictionId};

/// The most bytes a catalog filter holds.
pub const MAX_FILTER_BYTES: usize = 128;
/// The most models one catalog page holds.
pub const MAX_MODELS_PER_PAGE: u32 = 100;
/// The models a catalog page holds when the request names no limit.
pub const DEFAULT_MODELS_PER_PAGE: u32 = 20;

/// A resource address that failed admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaResourceError {
    Uri(ResourceUriError),
    UnknownResource,
    InvalidId,
    InvalidFilter,
    InvalidCursor,
}

impl fmt::Display for MediaResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uri(error) => error.fmt(f),
            Self::UnknownResource => f.write_str("unknown or noncanonical Media resource address"),
            Self::InvalidId => f.write_str("invalid Media model or prediction identity"),
            Self::InvalidFilter => f.write_str("invalid Media listing filter"),
            Self::InvalidCursor => f.write_str("invalid Media listing cursor"),
        }
    }
}

impl Error for MediaResourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Uri(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ResourceUriError> for MediaResourceError {
    fn from(error: ResourceUriError) -> Self {
        Self::Uri(error)
    }
}

/// A catalog filter: trimmed, lowercase, 1 to 128 bytes. The catalog matches a query
/// against a model's identity, name, type and description, and a type exactly.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CatalogFilter(String);

impl CatalogFilter {
    /// The normalized filter, or `None` for a blank one.
    pub fn parse(value: &str) -> Result<Option<Self>, MediaResourceError> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Ok(None);
        }
        if normalized.len() > MAX_FILTER_BYTES || normalized.chars().any(char::is_control) {
            return Err(MediaResourceError::InvalidFilter);
        }
        Ok(Some(Self(normalized)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CatalogFilter {
    type Error = MediaResourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)?.ok_or(MediaResourceError::InvalidFilter)
    }
}

impl From<CatalogFilter> for String {
    fn from(value: CatalogFilter) -> Self {
        value.0
    }
}

impl schemars::JsonSchema for CatalogFilter {
    fn schema_name() -> Cow<'static, str> {
        "CatalogFilter".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_FILTER_BYTES,
            "description": "A case-insensitive filter."
        })
    }
}

/// How many models a catalog page holds: 1 to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct CatalogLimit(u32);

impl CatalogLimit {
    pub fn new(value: u32) -> Result<Self, MediaResourceError> {
        (1..=MAX_MODELS_PER_PAGE)
            .contains(&value)
            .then_some(Self(value))
            .ok_or(MediaResourceError::InvalidFilter)
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for CatalogLimit {
    fn default() -> Self {
        Self(DEFAULT_MODELS_PER_PAGE)
    }
}

impl TryFrom<u32> for CatalogLimit {
    type Error = MediaResourceError;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CatalogLimit> for u32 {
    fn from(value: CatalogLimit) -> Self {
        value.0
    }
}

impl schemars::JsonSchema for CatalogLimit {
    fn schema_name() -> Cow<'static, str> {
        "CatalogLimit".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "minimum": 1,
            "maximum": MAX_MODELS_PER_PAGE,
            "description": "How many models the page holds; 20 by default."
        })
    }
}

/// A document the server embeds and serves under `media://docs/{doc_id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Vocabulary)]
pub enum MediaDocument {
    /// The agent work manual, `AGENTS.md`.
    Agents,
    /// The design, `DESIGN.md`.
    Design,
}

/// A Media resource. A usable address carries only the identities, filters and cursor of
/// its own family.
#[veoveo_types::resource_address(routes(MediaAddresses), schema = derived, wire, parse = borrowed, display)]
pub enum MediaResource {
    #[resource(template = "media://docs")]
    Docs,
    #[resource(template = "media://docs/{doc_id}")]
    Document(#[resource(variable = "doc_id", codec = MediaFieldCodec)] MediaDocument),
    #[resource(template = "media://contract")]
    Contract,
    #[resource(
        template = "media://models{?query,type,limit,cursor}",
        route_error = listing_route_error
    )]
    Models {
        #[resource(codec = MediaFieldCodec)]
        query: Option<CatalogFilter>,
        #[resource(variable = "type", codec = MediaFieldCodec)]
        model_type: Option<CatalogFilter>,
        #[resource(codec = MediaFieldCodec)]
        limit: Option<CatalogLimit>,
        #[resource(codec = MediaFieldCodec)]
        cursor: Option<ListCursor>,
    },
    #[resource(template = "media://model/{+model_id}")]
    Model(#[resource(variable = "model_id", tail, codec = ModelTailCodec)] MediaModelId),
    #[resource(template = "media://predictions{?cursor}", route_error = listing_route_error)]
    Predictions {
        #[resource(codec = MediaFieldCodec)]
        cursor: Option<ListCursor>,
    },
    #[resource(template = "media://prediction/{prediction_id}")]
    Prediction(#[resource(variable = "prediction_id", codec = MediaFieldCodec)] MediaPredictionId),
    #[resource(template = "media://prediction/{prediction_id}/result")]
    Result(#[resource(variable = "prediction_id", codec = MediaFieldCodec)] MediaPredictionId),
}

impl MediaResource {
    /// The first page of the model catalog.
    pub fn models() -> Self {
        Self::Models {
            query: None,
            model_type: None,
            limit: None,
            cursor: None,
        }
    }

    /// The first page of the caller's predictions.
    pub fn predictions() -> Self {
        Self::Predictions { cursor: None }
    }

    /// Predictions change as their generations progress, so the prediction listing's
    /// root and single predictions accept subscriptions. Catalogs, documents and
    /// finished results do not change.
    pub fn is_subscribable(&self) -> bool {
        match self {
            Self::Predictions { cursor } => cursor.is_none(),
            Self::Prediction(_) => true,
            Self::Docs
            | Self::Document(_)
            | Self::Contract
            | Self::Models { .. }
            | Self::Model(_)
            | Self::Result(_) => false,
        }
    }
}

fn route_error(error: ResourceRouteError) -> MediaResourceError {
    match error {
        ResourceRouteError::Uri(error) => MediaResourceError::Uri(error),
        _ => MediaResourceError::UnknownResource,
    }
}

fn listing_route_error(error: ResourceRouteError) -> MediaResourceError {
    match error {
        ResourceRouteError::Query => MediaResourceError::InvalidFilter,
        _ => route_error(error),
    }
}

struct MediaFieldCodec;

impl ResourceFieldCodec<MediaDocument> for MediaFieldCodec {
    type Error = MediaResourceError;
    fn parse(value: &str) -> Result<MediaDocument, Self::Error> {
        value
            .parse()
            .map_err(|_| MediaResourceError::UnknownResource)
    }
    fn text(value: &MediaDocument) -> Cow<'_, str> {
        value.as_str().into()
    }
}

impl ResourceFieldCodec<CatalogFilter> for MediaFieldCodec {
    type Error = MediaResourceError;
    fn parse(value: &str) -> Result<CatalogFilter, Self::Error> {
        // An address spells a filter in its normalized form only.
        let filter = CatalogFilter::parse(value)?.ok_or(MediaResourceError::InvalidFilter)?;
        (filter.as_str() == value)
            .then_some(filter)
            .ok_or(MediaResourceError::InvalidFilter)
    }
    fn text(value: &CatalogFilter) -> Cow<'_, str> {
        value.as_str().into()
    }
}

impl ResourceFieldCodec<CatalogLimit> for MediaFieldCodec {
    type Error = MediaResourceError;
    fn parse(value: &str) -> Result<CatalogLimit, Self::Error> {
        let parsed: u32 = value
            .parse()
            .map_err(|_| MediaResourceError::InvalidFilter)?;
        let limit = CatalogLimit::new(parsed)?;
        (limit.get().to_string() == value)
            .then_some(limit)
            .ok_or(MediaResourceError::InvalidFilter)
    }
    fn text(value: &CatalogLimit) -> Cow<'_, str> {
        value.get().to_string().into()
    }
}

impl ResourceFieldCodec<ListCursor> for MediaFieldCodec {
    type Error = MediaResourceError;
    fn parse(value: &str) -> Result<ListCursor, Self::Error> {
        ListCursor::parse(value).map_err(|_| MediaResourceError::InvalidCursor)
    }
    fn text(value: &ListCursor) -> Cow<'_, str> {
        value.as_str().into()
    }
}

impl ResourceFieldCodec<MediaPredictionId> for MediaFieldCodec {
    type Error = MediaResourceError;
    fn parse(value: &str) -> Result<MediaPredictionId, Self::Error> {
        MediaPredictionId::parse(value).map_err(|_| MediaResourceError::InvalidId)
    }
    fn text(value: &MediaPredictionId) -> Cow<'_, str> {
        value.as_str().into()
    }
}

/// A model's address takes the rest of the URI: one path segment per model component.
struct ModelTailCodec;

impl ResourceTailCodec<MediaModelId> for ModelTailCodec {
    type Error = MediaResourceError;
    fn parse(segments: &[Cow<'_, str>]) -> Result<MediaModelId, Self::Error> {
        if segments.is_empty() || segments.iter().any(|segment| segment.contains('/')) {
            return Err(MediaResourceError::InvalidId);
        }
        MediaModelId::parse(segments.join("/")).map_err(|_| MediaResourceError::InvalidId)
    }
    fn segments(value: &MediaModelId) -> Vec<Cow<'_, str>> {
        value.components().map(Cow::Borrowed).collect()
    }
}

#[doc(hidden)]
pub struct MediaAddresses;
impl veoveo_types::ResourceProfile for MediaAddresses {
    type Error = MediaResourceError;
    const PROFILE: veoveo_types::ResourceProfileSpec<Self::Error> =
        veoveo_types::ResourceProfileSpec {
            route_error: |_, error| route_error(error),
        };
}

#[cfg(test)]
mod tests {
    use super::*;
    use veoveo_types::ResourceAddress;

    fn parse(uri: &str) -> Result<MediaResource, MediaResourceError> {
        MediaResource::parse(uri)
    }

    #[test]
    fn addresses_round_trip_through_their_routes() {
        for uri in [
            "media://docs",
            "media://docs/agents",
            "media://contract",
            "media://models",
            "media://models?query=flux&type=text-to-image&limit=5&cursor=abc_-1",
            "media://model/owner/model",
            "media://model/a.b/c-d/e_f",
            "media://predictions",
            "media://predictions?cursor=abc",
            "media://prediction/pred-1",
            "media://prediction/pred-1/result",
        ] {
            let address = parse(uri).unwrap_or_else(|error| panic!("{uri}: {error}"));
            assert_eq!(address.to_uri().unwrap().as_str(), uri);
        }
    }

    #[test]
    fn a_model_address_holds_its_components_as_segments() {
        let model = MediaModelId::parse("owner/model/edit").unwrap();
        let address = MediaResource::Model(model.clone());
        assert_eq!(
            address.to_uri().unwrap().as_str(),
            "media://model/owner/model/edit"
        );
        assert_eq!(parse("media://model/owner/model/edit").unwrap(), address);
    }

    #[test]
    fn a_prediction_encodes_reserved_characters_in_one_segment() {
        let prediction = MediaPredictionId::parse("a b?c").unwrap();
        let uri = MediaResource::Prediction(prediction.clone())
            .to_uri()
            .unwrap();
        assert_eq!(
            parse(uri.as_str()).unwrap(),
            MediaResource::Prediction(prediction)
        );
        assert!(!uri.as_str().contains(' '));
    }

    #[test]
    fn malformed_addresses_are_refused() {
        for uri in [
            "media://model",
            "media://model/",
            "media://model/a//b",
            "media://model/a/../b",
            "media://models?limit=0",
            "media://models?limit=101",
            "media://models?limit=05",
            "media://models?query=UPPER",
            "media://models?unknown=1",
            "media://predictions?cursor=a=b",
            "media://prediction/a/b/c",
            "media://docs/unknown",
            "media://usage",
        ] {
            assert!(parse(uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn only_predictions_and_their_root_accept_subscriptions() {
        assert!(MediaResource::predictions().is_subscribable());
        assert!(
            MediaResource::Prediction(MediaPredictionId::parse("p").unwrap()).is_subscribable()
        );
        assert!(!MediaResource::models().is_subscribable());
        assert!(!MediaResource::Result(MediaPredictionId::parse("p").unwrap()).is_subscribable());
        assert!(
            !MediaResource::Predictions {
                cursor: Some(ListCursor::parse("abc").unwrap())
            }
            .is_subscribable()
        );
    }
}
