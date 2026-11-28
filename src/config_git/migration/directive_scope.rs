use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, de};
use time::OffsetDateTime;

use super::Reason;
use crate::dolt::is_uuid_v7;

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "scope_kind", rename_all = "snake_case")]
pub(crate) enum DirectiveScope {
    Area { area_id: String },
    Workspace,
    Project { project_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ScopeReview {
    Area {},
    Workspace {},
    Project {
        #[serde(deserialize_with = "project_id")]
        project_id: String,
    },
}

impl Default for ScopeReview {
    fn default() -> Self {
        Self::Area {}
    }
}

fn project_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let id = String::deserialize(deserializer)?;
    if !is_uuid_v7(&id) {
        return Err(de::Error::custom("expected a lowercase UUIDv7 Project ID"));
    }
    Ok(id)
}

pub(super) struct DirectiveContext<'a> {
    pub(super) observed_at: Option<OffsetDateTime>,
    pub(super) project_ids: HashSet<&'a str>,
}

impl ScopeReview {
    pub(super) fn resolve(
        &self,
        mapped_scope: &DirectiveScope,
        context: &DirectiveContext<'_>,
    ) -> Result<DirectiveScope, Reason> {
        match self {
            Self::Area {} => Ok(mapped_scope.clone()),
            Self::Workspace {} => Ok(DirectiveScope::Workspace),
            Self::Project { project_id } if context.project_ids.contains(project_id.as_str()) => {
                Ok(DirectiveScope::Project {
                    project_id: project_id.clone(),
                })
            }
            Self::Project { .. } => Err(Reason::UnresolvedDirectiveProject),
        }
    }
}
