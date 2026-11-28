use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::is_full_lowercase_oid;
use super::json_object;
use super::scope_plan::{PreparedResources, is_portable_scope};
use super::task_intake::{StagingTask, task_key_digest};
use super::task_provenance::{Key, LegacyKey};
use crate::cli::is_slug;
use crate::dolt::is_uuid_v7;
use crate::error::{ErrorCode, HiveError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    #[serde(deserialize_with = "json_object::deserialize")]
    legacy_key: LegacyKey,
    source_git_blob_sha: String,
    resource_id: String,
}

pub(super) fn optional_list<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Binding>>, D::Error> {
    json_object::deserialize_vec(deserializer).map(Some)
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Hold {
    TaskUnprepared,
    TaskBlobMismatch,
    ResourceUnprepared,
    ResourceAreaMismatch,
}

pub(crate) struct IssueResourceOutcome<'a> {
    pub(crate) key: Key<'a>,
    pub(crate) resource_id: &'a str,
    pub(crate) hold: Option<Hold>,
}

impl IssueResourceOutcome<'_> {
    pub(crate) fn report(&self) -> Value {
        let mut row = json!({
            "legacy_key_sha256": task_key_digest(self.key),
            "resource_id": self.resource_id,
            "status": if self.hold.is_some() { "held" } else { "prepared" },
        });
        if let Some(reason) = self.hold {
            row["reason_code"] = json!(reason);
        }
        row
    }
}

pub(super) fn prepare<'a>(
    bindings: &'a [Binding],
    tasks: Option<&[StagingTask<'_>]>,
    resources: Option<&PreparedResources<'_>>,
) -> Result<Vec<IssueResourceOutcome<'a>>, HiveError> {
    let tasks = tasks.ok_or_else(|| invalid("Issue Resource bindings require --tasks."))?;
    let task_by_key: HashMap<_, _> = tasks.iter().map(|task| (task.key, task)).collect();
    let area_by_resource: HashMap<_, _> = resources
        .into_iter()
        .flat_map(PreparedResources::outcomes)
        .filter_map(|(id, outcome)| outcome.ok().map(|binding| (id, binding.link.area_id)))
        .collect();
    let mut pairs = HashSet::new();
    bindings
        .iter()
        .map(|binding| {
            let key = binding.legacy_key.key();
            let resource_id = binding.resource_id.as_str();
            if !is_portable_scope(key.0)
                || !is_slug(key.1)
                || !is_uuid_v7(resource_id)
                || !is_full_lowercase_oid(&binding.source_git_blob_sha)
                || !pairs.insert((key, resource_id))
            {
                return Err(invalid(
                    "Issue Resource bindings have invalid or repeated endpoints.",
                ));
            }
            let hold = match task_by_key.get(&key) {
                None => Some(Hold::TaskUnprepared),
                Some(task) if task.blob_oid != binding.source_git_blob_sha => {
                    Some(Hold::TaskBlobMismatch)
                }
                Some(task) => match area_by_resource.get(resource_id) {
                    None => Some(Hold::ResourceUnprepared),
                    // Mapped scope Areas are unique, so a matching Area proves scope ownership.
                    Some(area) if *area != task.area_id => Some(Hold::ResourceAreaMismatch),
                    Some(_) => None,
                },
            };
            Ok(IssueResourceOutcome {
                key,
                resource_id,
                hold,
            })
        })
        .collect()
}

fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidConfig, message)
}
