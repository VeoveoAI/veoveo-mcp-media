//! Generations: the arguments of `run`, a prediction as Media tracks it, and the result a
//! finished run returns. A result links its outputs by their Artifact addresses and never
//! carries a download URL.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use veoveo_types::{ResourceUri, TaskId, Vocabulary};

use super::{MediaModelId, MediaPredictionId, MediaResource};

/// The format of a generation result.
pub const GENERATION_FORMAT: &str = "https://veoveo.ai/formats/media-generation";
/// The text a finished run returns beside its structured result.
pub const GENERATION_COMPLETED: &str = "Generation completed.";

/// The arguments of `run`, which runs as a Task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[veoveo_hub_sdk::tool(provider = super::MediaProvider, output = veoveo_hub_sdk::definition::Product<GenerationResult>, name = "run", task = true)]
pub struct RunRequest {
    /// The model to run. Browse `media://models` or complete the model template.
    pub model: MediaModelId,
    /// The model's input, which must satisfy the schema at `media://model/{+model_id}`.
    pub input: Map<String, Value>,
}

/// Where a generation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Vocabulary)]
pub enum PredictionStatus {
    /// The run waits for a worker.
    Queued,
    /// The run works: it checks and prices its input, or waits for the provider's
    /// outcome.
    Processing,
    /// The provider finished, and Media stores the outputs in Artifact.
    Storing,
    /// The caller cancelled the run, and the cancellation is on its way to the provider.
    Cancelling,
    /// Every output is stored.
    Succeeded,
    /// The provider or Media could not finish the run.
    Failed,
    /// The run was cancelled.
    Cancelled,
    /// The provider reported no outcome before the deadline. A late report still
    /// settles the provider's job, but the run has ended.
    Unresolved,
}

impl PredictionStatus {
    /// Whether the run has ended.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Unresolved
        )
    }
}

/// How the provider's outcome reaches a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Vocabulary)]
pub enum CompletionMode {
    /// The provider posts its signed webhook to the run's callback under the
    /// installation's public origin.
    Events,
    /// Media reads the prediction's status endpoint on a bounded schedule.
    Checks,
}

/// Why a run failed: the error code its Task records, beside a message that names no
/// secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Vocabulary)]
pub enum RunFailureCode {
    /// The catalog could not be read when the run started.
    CatalogUnavailable,
    /// The catalog holds no such model.
    UnknownModel,
    /// The model's schema refuses the input.
    InvalidInput,
    /// The provider could not quote the run's price, so the run never reached it.
    PriceUnavailable,
    /// The provider refused the submission.
    ProviderRefused,
    /// The provider named a prediction that another run already holds.
    PredictionConflict,
    /// The provider failed, timed out, cancelled or deleted the prediction.
    GenerationFailed,
    /// The provider reported no outcome before the deadline.
    GenerationUnresolved,
    /// An output could not be fetched or stored in Artifact.
    OutputNotStored,
    /// The installation database refused a write the run needed.
    StoreRefused,
    /// The run stopped on an error Media does not classify. Media's log holds its details.
    RunError,
}

/// A prediction as Media tracks it, without the run's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredictionSummary {
    /// The Task that runs the generation.
    #[schemars(with = "String")]
    pub task_id: TaskId,
    /// The provider's identity of the generation, once the provider accepted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction_id: Option<MediaPredictionId>,
    pub model_id: MediaModelId,
    pub status: PredictionStatus,
    /// How the provider's outcome reaches the run, once the run reached the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<CompletionMode>,
    /// Why the run failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub output_count: u32,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// One output of a generation, stored in Artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedArtifact {
    /// The output's position in the provider's output order.
    pub index: u32,
    /// The artifact's address, `artifact://artifacts/{artifact_id}`. Read it, or call
    /// Artifact's `create_download`, to reach the bytes.
    #[schemars(with = "String")]
    pub artifact_uri: ResourceUri,
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// What a finished run returns: the prediction, its result address and its outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(veoveo_hub_sdk::SingleOutput)]
pub struct GenerationResult {
    /// `https://veoveo.ai/formats/media-generation`.
    pub format: String,
    #[schemars(with = "String")]
    pub task_id: TaskId,
    pub prediction: PredictionSummary,
    pub artifacts: Vec<GeneratedArtifact>,
}

/// One prediction in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredictionEntry {
    /// `media://prediction/{prediction_id}`, once the provider accepted the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction_uri: Option<MediaResource>,
    pub prediction: PredictionSummary,
}

/// One prediction and, once it succeeded, its result's address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(veoveo_hub_sdk::SingleOutput)]
pub struct PredictionView {
    pub prediction: PredictionSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_uri: Option<MediaResource>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_codes_use_snake_case() {
        assert_eq!(
            RunFailureCode::PredictionConflict.as_str(),
            "prediction_conflict"
        );
        assert_eq!(
            RunFailureCode::GenerationUnresolved.as_str(),
            "generation_unresolved"
        );
        assert_eq!(
            "output_not_stored".parse::<RunFailureCode>().unwrap(),
            RunFailureCode::OutputNotStored
        );
    }

    #[test]
    fn statuses_use_snake_case_and_final_ones_end_the_run() {
        assert_eq!(PredictionStatus::Processing.as_str(), "processing");
        for status in PredictionStatus::ALL {
            assert_eq!(
                status.is_final(),
                !matches!(
                    status,
                    PredictionStatus::Queued
                        | PredictionStatus::Processing
                        | PredictionStatus::Storing
                        | PredictionStatus::Cancelling
                ),
                "{status}"
            );
        }
    }
}
