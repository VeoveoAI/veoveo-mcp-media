//! Media's OAuth scope and permissions. Each permission names one action on one resource
//! type, with the access kind and risks the `ai.veoveo/authorization` declaration
//! carries; installation roles grant them and the Hub forwards the granted ones.
use veoveo_hub_sdk::registration::{Access, PermissionDefinition, Risk};
use veoveo_types::{PermissionName, Vocabulary};

/// The OAuth scope that names Media's namespace. Scopes are coarse; Media's permissions
/// carry fine-grained access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Vocabulary)]
#[vocabulary(scope)]
pub enum MediaScope {
    #[vocabulary(rename = "media")]
    Media,
}

/// Media's permissions.
/// ```compile_fail
/// use veoveo_mcp_media::contract::MediaPermission;
/// fn requires(_: MediaPermission) {}
/// requires("media.models.read");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Vocabulary)]
pub enum MediaPermission {
    /// Browse the model catalog and read a model's input schema.
    #[vocabulary(rename = "media.models.read")]
    ModelsRead,
    /// Read your own predictions and their results.
    #[vocabulary(rename = "media.predictions.read")]
    PredictionsRead,
    /// Run a model, which the provider bills.
    #[vocabulary(rename = "media.generations.run")]
    GenerationsRun,
}

impl MediaPermission {
    /// The resource type the permission acts on.
    pub fn resource_type(self) -> &'static str {
        match self {
            Self::ModelsRead => "media.model",
            Self::PredictionsRead => "media.prediction",
            Self::GenerationsRun => "media.generation",
        }
    }

    /// The permission's wire name, as declared and as granted.
    pub fn name(self) -> PermissionName {
        PermissionName::parse(self.as_str()).expect("declared Media permission spelling")
    }
}

impl PermissionDefinition for MediaPermission {
    fn resource(self) -> &'static str {
        self.resource_type()
    }

    fn access(self) -> Access {
        match self {
            Self::ModelsRead | Self::PredictionsRead => Access::Read,
            Self::GenerationsRun => Access::Write,
        }
    }

    fn risks(self) -> &'static [Risk] {
        match self {
            Self::GenerationsRun => &[Risk::Billed],
            Self::ModelsRead | Self::PredictionsRead => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_follow_the_naming_form_and_only_running_is_billed() {
        for permission in MediaPermission::ALL {
            let name = permission.as_str();
            let mut parts = name.split('.');
            assert_eq!(parts.next(), Some("media"), "{name}");
            assert!(parts.next().is_some_and(|plural| !plural.is_empty()));
            assert!(parts.next().is_some_and(|verb| !verb.is_empty()));
            assert_eq!(parts.next(), None, "{name}");
            assert!(permission.resource_type().starts_with("media."));
            assert_eq!(
                permission.risks() == [Risk::Billed],
                *permission == MediaPermission::GenerationsRun
            );
        }
        assert_eq!(MediaScope::Media.as_str(), "media");
    }
}
