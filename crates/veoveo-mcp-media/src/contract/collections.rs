//! Prediction ordering and caller-bound live keysets; no Task runtime dependency.
use std::{cmp::Ordering, fmt};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use veoveo_hub_sdk::{HubProfileId, definition::NoFilters};
use veoveo_types::{
    Collection, CursorError, CursorScope, PageSize, PagingPolicy, PrincipalId, TaskId,
};

use super::PredictionEntry;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredictionPosition {
    pub created_at: DateTime<Utc>,
    pub task_id: TaskId,
}

/// Runs in descending creation-time order, with task identity as the unique tiebreaker.
pub struct PredictionCollection;
impl Collection for PredictionCollection {
    type Item = PredictionEntry;
    type Filters = NoFilters;
    type Key = PredictionPosition;
    type Limit = PageSize<100>;
    type Policy = PredictionKeyset;
    const NAME: &'static str = "media.predictions";
    fn key(item: &PredictionEntry) -> PredictionPosition {
        PredictionPosition {
            created_at: item.prediction.created_at,
            task_id: item.prediction.task_id,
        }
    }
    fn compare(_: &NoFilters, left: &PredictionPosition, right: &PredictionPosition) -> Ordering {
        right.cmp(left)
    }
}

/// Scope is captured from current admission; the Task query still applies current
/// owner, profile and labels on every page. A cursor carries no grants.
pub struct PredictionKeyset {
    principal: PrincipalId,
    profile: HubProfileId,
}
impl PredictionKeyset {
    pub fn new(principal: PrincipalId, profile: HubProfileId) -> Self {
        Self { principal, profile }
    }
}
impl PagingPolicy<PredictionCollection> for PredictionKeyset {
    fn scope(
        &self,
        tenant: impl fmt::Display,
        filters: &NoFilters,
        limit: u32,
    ) -> Result<CursorScope, CursorError> {
        CursorScope::new(
            PredictionCollection::NAME,
            tenant,
            &(filters, limit, &self.principal, &self.profile),
        )
    }
}
