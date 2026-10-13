//! Media's identities: provider models, provider predictions and the opaque cursors of its
//! listings. Each admits its input once, so internal code never handles a raw string
//! where a model or a prediction is meant.
use std::{borrow::Cow, fmt, str::FromStr};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// The most bytes a model identity holds.
pub const MAX_MODEL_ID_BYTES: usize = 512;
/// The most bytes a prediction identity holds.
pub const MAX_PREDICTION_ID_BYTES: usize = 512;
/// The most bytes a listing cursor holds on the wire.
pub const MAX_CURSOR_BYTES: usize = 4096;

/// A value that failed admission as one of Media's identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MediaIdError {
    #[error(
        "a model is 1 to 512 ASCII bytes of `/`-separated components of letters, digits, `-`, `_` and `.`"
    )]
    Model,
    #[error("a prediction is 1 to 512 bytes without `/` or control characters")]
    Prediction,
    #[error("a cursor is 1 to 4096 base64url characters from the previous page")]
    Cursor,
}

/// A provider model, such as `owner/model/variant`: up to 512 ASCII bytes of non-empty
/// components separated by `/`, each of letters, digits, `-`, `_` and `.`, and never
/// `.` or `..`. Its address is `media://model/{+model_id}`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MediaModelId(String);

impl MediaModelId {
    pub fn parse(value: impl Into<String>) -> Result<Self, MediaIdError> {
        let value = value.into();
        let admitted = !value.is_empty()
            && value.len() <= MAX_MODEL_ID_BYTES
            && value.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            });
        admitted.then_some(Self(value)).ok_or(MediaIdError::Model)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The model's `/`-separated components, as its address and the provider's submit
    /// path spell them.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for MediaModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MediaModelId {
    type Err = MediaIdError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for MediaModelId {
    type Error = MediaIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<MediaModelId> for String {
    fn from(value: MediaModelId) -> Self {
        value.0
    }
}

impl JsonSchema for MediaModelId {
    fn schema_name() -> Cow<'static, str> {
        "MediaModelId".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_MODEL_ID_BYTES,
            "pattern": "^[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)*$",
            "description": "A model, such as `owner/model`. Browse `media://models` for models."
        })
    }
}

/// A provider prediction: the provider's own identity of one generation, 1 to 512 bytes
/// without `/` or control characters. Its address is `media://prediction/{id}`, one
/// path segment.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MediaPredictionId(String);

impl MediaPredictionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, MediaIdError> {
        let value = value.into();
        let admitted = !value.is_empty()
            && value.len() <= MAX_PREDICTION_ID_BYTES
            && !value.contains('/')
            && !value.chars().any(char::is_control);
        admitted
            .then_some(Self(value))
            .ok_or(MediaIdError::Prediction)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MediaPredictionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MediaPredictionId {
    type Err = MediaIdError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for MediaPredictionId {
    type Error = MediaIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<MediaPredictionId> for String {
    fn from(value: MediaPredictionId) -> Self {
        value.0
    }
}

impl JsonSchema for MediaPredictionId {
    fn schema_name() -> Cow<'static, str> {
        "MediaPredictionId".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_PREDICTION_ID_BYTES,
            "pattern": "^[^/\\u0000-\\u001f\\u007f]+$",
            "description": "The provider's identity of one generation."
        })
    }
}

/// An opaque listing cursor as it travels in an address or a request: 1 to 4096
/// base64url characters. The server admits it against the listing, tenant, filters and,
/// for models, the catalog snapshot that issued it; this type checks only its spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ListCursor(String);

impl ListCursor {
    pub fn parse(value: impl Into<String>) -> Result<Self, MediaIdError> {
        let value = value.into();
        let spelled = !value.is_empty()
            && value.len() <= MAX_CURSOR_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        spelled.then_some(Self(value)).ok_or(MediaIdError::Cursor)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ListCursor {
    type Error = MediaIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ListCursor> for String {
    fn from(value: ListCursor) -> Self {
        value.0
    }
}

impl fmt::Display for ListCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl JsonSchema for ListCursor {
    fn schema_name() -> Cow<'static, str> {
        "ListCursor".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_CURSOR_BYTES,
            "pattern": "^[A-Za-z0-9_-]+$",
            "description": "An opaque cursor from the previous page of the same listing."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_admit_slash_separated_safe_components() {
        for model in ["owner/model", "a", "a.b/c-d/e_f", "x/1.5"] {
            assert_eq!(MediaModelId::parse(model).unwrap().as_str(), model);
        }
        let long = "a".repeat(MAX_MODEL_ID_BYTES + 1);
        for model in [
            "",
            "/",
            "a/",
            "/a",
            "a//b",
            "a/./b",
            "a/../b",
            "a b",
            "a?b",
            "é",
            long.as_str(),
        ] {
            assert_eq!(
                MediaModelId::parse(model),
                Err(MediaIdError::Model),
                "{model:?}"
            );
        }
        assert_eq!(
            MediaModelId::parse("owner/model/edit")
                .unwrap()
                .components()
                .collect::<Vec<_>>(),
            ["owner", "model", "edit"]
        );
    }

    #[test]
    fn predictions_are_one_segment_without_controls() {
        for prediction in ["abc", "pred-1.2", "ünïcode", "with space", "a?b#c"] {
            assert!(
                MediaPredictionId::parse(prediction).is_ok(),
                "{prediction:?}"
            );
        }
        let long = "p".repeat(MAX_PREDICTION_ID_BYTES + 1);
        for prediction in ["", "a/b", "a\nb", "\u{7f}", long.as_str()] {
            assert_eq!(
                MediaPredictionId::parse(prediction),
                Err(MediaIdError::Prediction),
                "{prediction:?}"
            );
        }
    }

    #[test]
    fn cursors_check_their_spelling_only() {
        assert!(ListCursor::parse("eyJh-_0").is_ok());
        for cursor in ["", "a=b", "a+b", "a b"] {
            assert_eq!(ListCursor::parse(cursor), Err(MediaIdError::Cursor));
        }
    }
}
