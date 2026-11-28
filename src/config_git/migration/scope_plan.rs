use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use super::queen_assets::{self, SoulReview};
use super::{AssetObjectReader, Inventory, Reason, snapshot_object, verified_source};
use crate::cli::is_slug;
use crate::config_file::{MAX_FILE_BYTES, read_regular};
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
    queen_assets: Option<Vec<SoulReview>>,
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

#[derive(Deserialize)]
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
    canonical_ref: String,
}

struct Scope<'a> {
    scope: &'a str,
    source_paths: Vec<&'a str>,
    outcome: ScopeOutcome<'a>,
}

pub(super) enum ScopeOutcome<'a> {
    Mapped { area: &'a Area },
    Held { reason: Reason },
    Excluded { reason: Reason },
}

impl Scope<'_> {
    fn report(&self) -> Value {
        let outcome = match self.outcome {
            ScopeOutcome::Mapped { area } => json!({
                "status": "mapped",
                "area": {"id": area.id, "slug": area.slug, "parent_id": area.parent_id},
            }),
            ScopeOutcome::Held { reason } => json!({"status": "held", "reason": reason}),
            ScopeOutcome::Excluded { reason } => json!({"status": "excluded", "reason": reason}),
        };
        json!({"scope": self.scope, "source_paths": self.source_paths, "outcome": outcome})
    }
}

pub(crate) fn plan(
    source: &Path,
    revision: &str,
    workspace_id: &str,
    mapping_file: &Path,
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

    verified_source(source, revision, |source, inventory, reader| {
        if mapping.source_commit != source.source_commit
            || mapping.policy_pin != source.policy_pin
            || mapping.workspace_id != workspace_id
        {
            return Err(invalid(
                "The scope mapping must match the verified source commit, policy pin, and selected workspace ID.",
            ));
        }

        let scopes = classify_scopes(inventory, &mapping.scopes, reader)?;
        let mut report = json!({
            "mode": "scope_plan",
            "mapping_version": mapping.version,
            "mapping_sha256": format!("{:x}", Sha256::digest(input)),
            "source": source,
            "inventory": inventory.summary,
            "target": {"workspace_id": workspace_id},
            "scopes": scopes.iter().map(Scope::report).collect::<Vec<_>>(),
        });
        if let Some(reviews) = &mapping.queen_assets {
            report["queen_assets"] = json!(queen_assets::classify(inventory, reader, reviews)?);
        }
        Ok(report)
    })
}

fn classify_scopes<'a>(
    inventory: &'a Inventory,
    mappings: &'a [ScopeMapping],
    reader: &AssetObjectReader<'_>,
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

    let mut references = HashMap::new();
    scopes
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
                        || project_agrees(
                            inventory,
                            &origins,
                            project_evidence,
                            reader,
                            &mut references,
                        )?) =>
                {
                    ScopeOutcome::Mapped { area }
                }
                _ => ScopeOutcome::Held {
                    reason: Reason::AmbiguousScopeOrCollision,
                },
            };
            Ok(Scope {
                scope,
                source_paths: origins.into_iter().map(|(path, _)| path).collect(),
                outcome,
            })
        })
        .collect()
}

fn project_agrees<'a>(
    inventory: &'a Inventory,
    origins: &[(&str, &str)],
    evidence: &[ProjectEvidence],
    reader: &AssetObjectReader<'_>,
    references: &mut HashMap<&'a str, Option<String>>,
) -> Result<bool, HiveError> {
    let Some(first) = evidence.first() else {
        return Ok(false);
    };
    let paths: BTreeSet<_> = evidence.iter().map(|item| item.path.as_str()).collect();
    if evidence.len() != origins.len()
        || paths.len() != origins.len()
        || !portable_reference(&first.canonical_ref)
        || evidence
            .iter()
            .any(|item| item.canonical_ref != first.canonical_ref)
    {
        return Ok(false);
    }
    for (directory, entrypoint) in origins {
        let path = format!("{directory}/{entrypoint}");
        if !paths.contains(path.as_str()) {
            return Ok(false);
        }
        let Some(entry) = inventory.entries.get(&path) else {
            return Ok(false);
        };
        let Some(size) = entry.size else {
            return Ok(false);
        };
        let reference = match references.entry(entry.oid.as_str()) {
            std::collections::hash_map::Entry::Occupied(cached) => cached.into_mut(),
            std::collections::hash_map::Entry::Vacant(cache) => {
                // Read by catalog object ID inside the same preflight checks.
                let blob = snapshot_object(&|| reader.command(), "blob", &entry.oid, size)?;
                cache.insert(entrypoint_reference(&blob).map(str::to_owned))
            }
        };
        if reference.as_deref() != Some(first.canonical_ref.as_str()) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn entrypoint_reference(bytes: &[u8]) -> Option<&str> {
    let mut lines = std::str::from_utf8(bytes).ok()?.lines().peekable();
    if lines.peek() == Some(&"---") {
        lines.next();
        lines.by_ref().find(|line| *line == "---")?;
    }
    // V1 uses prose for identity. Accept simple standalone URLs and hold when
    // the text contains multiple distinct URL candidates.
    let mut candidates = lines
        .flat_map(str::split_whitespace)
        .filter(|token| token.contains("://"))
        .map(url_token);
    let reference = candidates.next()?;
    (portable_reference(reference) && candidates.all(|other| other == reference))
        .then_some(reference)
}

fn url_token(token: &str) -> &str {
    let wrapped = token.trim_end_matches(['.', ',', ':', ';', '!', '?']);
    if let Some((label, target)) = wrapped.split_once("](")
        && !label.contains("://")
        && let Some(target) = target.strip_suffix(')')
    {
        return target;
    }
    for (open, close) in [('<', '>'), ('(', ')'), ('`', '`'), ('"', '"'), ('\'', '\'')] {
        if let Some(target) = wrapped
            .strip_prefix(open)
            .and_then(|target| target.strip_suffix(close))
        {
            return target;
        }
    }
    token
}

fn portable_reference(reference: &str) -> bool {
    if reference.len() > MAX_FILE_BYTES
        || !reference.starts_with("https://")
        || !reference.is_ascii()
        || reference.contains('@')
        || reference.matches("://").count() != 1
    {
        return false;
    }
    let Ok(url) = Url::parse(reference) else {
        return false;
    };
    let Some(Host::Domain(host)) = url.host() else {
        return false;
    };
    url.as_str() == reference
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() != "/"
        && host.contains('.')
        && host.split('.').all(|label| {
            label.starts_with(|c: char| c.is_ascii_alphanumeric())
                && label.ends_with(|c: char| c.is_ascii_alphanumeric())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && ![".local", ".localhost", ".internal", ".lan", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn reserved(scope: &str) -> bool {
    matches!(scope, "_general" | "_hive-userdata")
}

fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidConfig, message)
}
