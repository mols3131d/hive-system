use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Inventory, Reason, task_intake, verified_source};
use crate::cli::is_slug;
use crate::config_file::read_regular;
use crate::dolt::{is_uuid, is_uuid_v7};
use crate::error::{ErrorCode, HiveError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    version: u32,
    source_commit: String,
    policy_pin: String,
    workspace_id: String,
    scopes: Vec<ScopeMapping>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeMapping {
    scope: String,
    decision: Decision,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Decision {
    Area {
        area: Area,
        #[serde(default)]
        project_evidence: Vec<ProjectEvidence>,
    },
    External {},
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Area {
    pub(super) id: String,
    slug: String,
    name: String,
    parent_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectEvidence {
    path: String,
    project_key: String,
}

#[derive(Serialize)]
struct Scope<'a> {
    scope: &'a str,
    source_paths: Vec<&'a str>,
    outcome: ScopeOutcome<'a>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum ScopeOutcome<'a> {
    Mapped { area: &'a Area },
    Held { reason: Reason },
    Excluded { reason: Reason },
}

pub(crate) fn plan(
    source: &Path,
    revision: &str,
    workspace_id: &str,
    mapping_file: &Path,
    include_tasks: bool,
) -> Result<Value, HiveError> {
    if !is_uuid(workspace_id) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Select an exact lowercase --workspace-id UUID.",
        ));
    }
    let input = read_regular(mapping_file)
        .map_err(|error| HiveError::new(error.code, "Could not read the scope mapping file."))?
        .ok_or_else(|| invalid("The scope mapping file is missing."))?;
    let mapping: Mapping = serde_json::from_slice(&input)
        .map_err(|_| invalid("The scope mapping must match the versioned JSON format."))?;
    if mapping.version != 1 {
        return Err(invalid("The scope mapping version is unsupported."));
    }

    let (source, inventory, tasks) = verified_source(source, revision, include_tasks)?;
    if mapping.source_commit != source.source_commit
        || mapping.policy_pin != source.policy_pin
        || mapping.workspace_id != workspace_id
    {
        return Err(invalid(
            "The scope mapping must match the verified source commit, policy pin, and selected workspace ID.",
        ));
    }

    let scopes = classify_scopes(&inventory, &mapping.scopes)?;
    let mut report = json!({
        "mode": "scope_plan",
        "mapping_version": mapping.version,
        "mapping_sha256": format!("{:x}", Sha256::digest(input)),
        "source": source,
        "inventory": inventory.summary,
        "target": {"workspace_id": workspace_id},
        "scopes": scopes,
    });
    if include_tasks {
        let outcomes = scopes
            .iter()
            .map(|scope| (scope.scope, &scope.outcome))
            .collect();
        report["task_intake"] = task_intake::report(tasks, &outcomes);
    }
    Ok(report)
}

fn classify_scopes<'a>(
    inventory: &'a Inventory,
    mappings: &'a [ScopeMapping],
) -> Result<Vec<Scope<'a>>, HiveError> {
    let mut scopes = BTreeMap::<&str, Vec<(&str, &str)>>::new();
    for (path, entry) in &inventory.entries {
        if !entry.is_directory() {
            continue;
        }
        let components: Vec<_> = path.split('/').take(5).collect();
        let (scope, entrypoint) = match components.as_slice() {
            ["tasks" | "directives", scope] => (*scope, "README.md"),
            ["bots", _, "memory", scope] => (*scope, "MEMORY.md"),
            _ => continue,
        };
        scopes.entry(scope).or_default().push((path, entrypoint));
    }

    let mut decisions = HashMap::<&str, Vec<&Decision>>::new();
    let mut ids = HashMap::new();
    let mut siblings = HashMap::new();
    for mapping in mappings {
        let scope = mapping.scope.as_str();
        if !(reserved(scope) || scope == "_hive" || is_slug(scope)) || !scopes.contains_key(scope) {
            return Err(invalid(
                "A scope mapping key does not name a supported scope in the verified source.",
            ));
        }
        if let Decision::Area {
            area,
            project_evidence,
        } = &mapping.decision
        {
            if !is_uuid_v7(&area.id)
                || area
                    .parent_id
                    .as_ref()
                    .is_some_and(|id| !is_uuid_v7(id) || *id == area.id)
                || area.slug.len() > 100
                || !is_slug(&area.slug)
                || area.name.trim().is_empty()
                || area.name.chars().count() > 255
                || area.name.chars().any(char::is_control)
                || (reserved(scope) && !project_evidence.is_empty())
                || project_evidence.iter().any(|evidence| {
                    evidence.project_key.len() > 100 || !is_slug(&evidence.project_key)
                })
            {
                return Err(invalid(
                    "A scope mapping has invalid Area or project evidence fields.",
                ));
            }
            if scope != "_hive" {
                *ids.entry(area.id.as_str()).or_insert(0) += 1;
                *siblings
                    .entry((area.parent_id.as_deref(), area.slug.as_str()))
                    .or_insert(0) += 1;
            }
        }
        decisions.entry(scope).or_default().push(&mapping.decision);
    }

    Ok(scopes
        .into_iter()
        .map(|(scope, origins)| {
            let decision = decisions.get(scope).map(Vec::as_slice);
            let outcome = match (scope, decision) {
                ("_hive", _) => ScopeOutcome::Held {
                    reason: Reason::UnsplitLegacyScope,
                },
                (_, Some([Decision::External {}])) if !reserved(scope) => ScopeOutcome::Excluded {
                    reason: Reason::ExternalBackendOwned,
                },
                (
                    _,
                    Some(
                        [
                            Decision::Area {
                                area,
                                project_evidence,
                            },
                        ],
                    ),
                ) if ids[area.id.as_str()] == 1
                    && siblings[&(area.parent_id.as_deref(), area.slug.as_str())] == 1
                    && (reserved(scope)
                        || project_agrees(inventory, &origins, project_evidence)) =>
                {
                    ScopeOutcome::Mapped { area }
                }
                _ => ScopeOutcome::Held {
                    reason: Reason::AmbiguousScopeOrCollision,
                },
            };
            Scope {
                scope,
                source_paths: origins.into_iter().map(|(path, _)| path).collect(),
                outcome,
            }
        })
        .collect())
}

fn project_agrees(
    inventory: &Inventory,
    origins: &[(&str, &str)],
    evidence: &[ProjectEvidence],
) -> bool {
    let Some(first) = evidence.first() else {
        return false;
    };
    let paths: BTreeSet<_> = evidence.iter().map(|item| item.path.as_str()).collect();
    evidence.len() == origins.len()
        && paths.len() == origins.len()
        && evidence
            .iter()
            .all(|item| item.project_key == first.project_key)
        && origins.iter().all(|(directory, entrypoint)| {
            let path = format!("{directory}/{entrypoint}");
            paths.contains(path.as_str())
                && inventory
                    .entries
                    .get(&path)
                    .is_some_and(|entry| !entry.is_directory())
        })
}

fn reserved(scope: &str) -> bool {
    matches!(scope, "_general" | "_hive-userdata")
}

fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidConfig, message)
}
