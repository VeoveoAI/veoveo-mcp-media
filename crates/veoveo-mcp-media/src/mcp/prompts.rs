//! Media's prompts: guidance for choosing a model and drafting a run, and for reviewing a
//! finished run. They read through resources and act through tools, so a prompt grants
//! nothing itself. Ported from the monorepo's Media server at 0c33cf8a7.
use rmcp::{
    ErrorData as McpError,
    model::{GetPromptResult, JsonObject, Prompt, PromptArgument, PromptMessage, Role},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use veoveo_types::TaskId;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSelectArgs {
    goal: String,
    media_type: Option<String>,
    budget: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageEditArgs {
    image_url: String,
    edit_goal: String,
    constraints: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VideoArgs {
    brief: String,
    reference_url: Option<String>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskReviewArgs {
    task_id: TaskId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPrompt {
    ModelSelect,
    ImageEdit,
    VideoGenerate,
    TaskReview,
}

impl MediaPrompt {
    pub const ALL: [Self; 4] = [
        Self::ModelSelect,
        Self::ImageEdit,
        Self::VideoGenerate,
        Self::TaskReview,
    ];

    pub fn by_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|prompt| prompt.name() == name)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::ModelSelect => "media-model-select",
            Self::ImageEdit => "media-image-edit",
            Self::VideoGenerate => "media-video-generate",
            Self::TaskReview => "media-task-review",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::ModelSelect => "Media model selection",
            Self::ImageEdit => "Image edit request",
            Self::VideoGenerate => "Video generation request",
            Self::TaskReview => "Media task review",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::ModelSelect => "Select media models and draft valid run arguments for a goal.",
            Self::ImageEdit => "Draft an image edit request using a source image URL.",
            Self::VideoGenerate => "Draft a video generation request from a creative brief.",
            Self::TaskReview => "Review a finished run's outputs and cost.",
        }
    }

    fn arguments(self) -> Vec<PromptArgument> {
        let required = |name: &str, description: &str| {
            PromptArgument::new(name)
                .with_description(description)
                .with_required(true)
        };
        let optional = |name: &str, description: &str| {
            PromptArgument::new(name)
                .with_description(description)
                .with_required(false)
        };
        match self {
            Self::ModelSelect => vec![
                required("goal", "What the generation should achieve."),
                optional("media_type", "Desired output: image, video, audio or 3D."),
                optional("budget", "Budget or cost guidance for model selection."),
            ],
            Self::ImageEdit => vec![
                required("image_url", "Public URL of the source image."),
                required("edit_goal", "The visual change to make."),
                optional(
                    "constraints",
                    "Style, brand, safety or composition constraints.",
                ),
            ],
            Self::VideoGenerate => vec![
                required("brief", "Creative brief for the video."),
                optional("reference_url", "Public image or video reference URL."),
                optional("duration", "Desired duration."),
            ],
            Self::TaskReview => vec![required("task_id", "The Task `run` started.")],
        }
    }

    pub fn definition(self) -> Prompt {
        Prompt::new(
            self.name(),
            Some(self.description()),
            Some(self.arguments()),
        )
        .with_title(self.title())
    }

    pub fn render(self, arguments: Option<JsonObject>) -> Result<GetPromptResult, McpError> {
        let text = match self {
            Self::ModelSelect => {
                let args: ModelSelectArgs = parse(self.name(), arguments)?;
                format!(
                    "Prepare a media model selection for this goal:\n\n\
                     Goal: {}\nMedia type: {}\nBudget guidance: {}\n\n\
                     Read media://models, choose the best candidate models, then read \
                     media://model/{{+model_id}} for each candidate before drafting run \
                     arguments. Return the selected model and a JSON input object that \
                     satisfies its schema exactly.",
                    args.goal,
                    args.media_type.as_deref().unwrap_or("not specified"),
                    args.budget.as_deref().unwrap_or("not specified"),
                )
            }
            Self::ImageEdit => {
                let args: ImageEditArgs = parse(self.name(), arguments)?;
                format!(
                    "Draft an image edit run.\n\n\
                     Source image URL: {}\nEdit goal: {}\nConstraints: {}\n\n\
                     Read media://models and prefer an image edit or image-to-image model. \
                     Then read media://model/{{+model_id}} and return only the model plus an \
                     input object that satisfies its schema.",
                    args.image_url,
                    args.edit_goal,
                    args.constraints.as_deref().unwrap_or("not specified"),
                )
            }
            Self::VideoGenerate => {
                let args: VideoArgs = parse(self.name(), arguments)?;
                format!(
                    "Draft a video generation run.\n\n\
                     Brief: {}\nReference URL: {}\nDuration guidance: {}\n\n\
                     Read media://models and choose a video model. Then read \
                     media://model/{{+model_id}} and return only the model plus an input \
                     object that satisfies its schema.",
                    args.brief,
                    args.reference_url.as_deref().unwrap_or("not specified"),
                    args.duration.as_deref().unwrap_or("not specified"),
                )
            }
            Self::TaskReview => {
                let args: TaskReviewArgs = parse(self.name(), arguments)?;
                format!(
                    "Review Media run {}.\n\n\
                     Read tasks/get for its status and result. The result names \
                     media://prediction/{{prediction_id}}/result and the Artifact address of \
                     each output, and the Task's `ai.veoveo/usage` metadata holds its \
                     estimated and actual cost. Summarize the outputs, their types and the \
                     final cost, and say when no actual cost was recorded.",
                    args.task_id,
                )
            }
        };
        Ok(
            GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                .with_description(self.description()),
        )
    }
}

fn parse<T: DeserializeOwned>(name: &str, arguments: Option<JsonObject>) -> Result<T, McpError> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default())).map_err(|error| {
        McpError::invalid_params(
            format!("invalid arguments for prompt {name}: {error}"),
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_render_with_their_required_arguments() {
        for prompt in MediaPrompt::ALL {
            assert_eq!(MediaPrompt::by_name(prompt.name()), Some(prompt));
            assert!(
                prompt.render(None).is_err(),
                "{} needs arguments",
                prompt.name()
            );
        }
        let task = TaskId::new();
        let review = MediaPrompt::TaskReview
            .render(Some(
                serde_json::json!({"task_id": task.to_string()})
                    .as_object()
                    .unwrap()
                    .clone(),
            ))
            .unwrap();
        let text = serde_json::to_string(&review).unwrap();
        assert!(text.contains(&task.to_string()));
        assert!(text.contains("ai.veoveo/usage"));
    }
}
