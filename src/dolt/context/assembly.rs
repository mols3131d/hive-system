//! Assemble an inspection result after the caller has verified runtime pairing
//! and selected the committed Role and (when required) Soul. This does not
//! establish actor authority or open another Snapshot.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::UtcOffset;

use super::{
    DEFAULT_INLINE_BYTES, MAX_INLINE_BYTES, Scope, Snapshot, collect_work, directives, is_uuid_v7,
    limit, references, resume, workflow,
};
use crate::cli::ContextRole;
use crate::dolt::DoltSuccess;
use crate::error::{ErrorCode, HiveError};

const MAX_JSON_BYTES: usize = 512 * 1024;

pub(super) struct RoleBody<'a> {
    pub(super) role: ContextRole,
    pub(super) policy_revision: &'a str,
    pub(super) body: &'a str,
}

pub(super) struct SoulBody<'a> {
    pub(super) workspace_id: &'a str,
    pub(super) db_commit: &'a str,
    pub(super) asset_commit: &'a str,
    pub(super) bot_id: &'a str,
    pub(super) bot_slug: &'a str,
    pub(super) registry_revision: &'a str,
    pub(super) body: &'a str,
}

pub(super) struct SelectedArtifact<'a> {
    pub(super) id: &'a str,
    pub(super) evidence: workflow::CurrentInput<'a>,
}

pub(super) struct Inputs<'a> {
    pub(super) role: ContextRole,
    pub(super) policy_revision: &'a str,
    pub(super) asset_commit: &'a str,
    pub(super) role_body: RoleBody<'a>,
    pub(super) soul: Option<SoulBody<'a>>,
    pub(super) request: workflow::Request<'a>,
    // Mapping from each exact applicable Directive to the historical activation
    // receipt selected by the trusted provenance path. No inferred approval.
    pub(super) activation_receipts: &'a BTreeMap<&'a str, &'a str>,
    pub(super) artifacts: &'a [SelectedArtifact<'a>],
    pub(super) budget: Option<usize>,
    pub(super) cursor: Option<&'a str>,
    pub(super) evidence_fingerprint: &'a str,
}

pub(super) fn assemble(
    snapshot: &mut Snapshot,
    scope: &Scope,
    inputs: &Inputs<'_>,
) -> Result<DoltSuccess, HiveError> {
    if scope.db_commit != snapshot.commit {
        return Err(HiveError::new(
            ErrorCode::StaleRevision,
            "Resolve context from one committed Snapshot.",
        ));
    }
    let max = inputs.budget.unwrap_or(DEFAULT_INLINE_BYTES);
    if !(1..=MAX_INLINE_BYTES).contains(&max)
        || inputs.artifacts.len() > crate::cli::MAX_CONTEXT_ARTIFACTS
        || inputs.activation_receipts.len() > directives::MAX_DIRECTIVES
    {
        return Err(limit("Context selection exceeds its request limits."));
    }
    let mut selected = BTreeSet::new();
    if inputs
        .artifacts
        .iter()
        .any(|entry| !is_uuid_v7(entry.id) || !selected.insert(entry.id))
    {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Select each exact Artifact ID at most once.",
        ));
    }
    let mut remaining = max;
    let identity = identity(snapshot, inputs, &mut remaining)?;
    let candidates = directives::collect(snapshot, scope, inputs.request.observed_at)?;
    let applicable: BTreeSet<_> = candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect();
    if inputs
        .activation_receipts
        .keys()
        .any(|id| !applicable.contains(id))
    {
        return Err(HiveError::new(
            ErrorCode::Unauthorized,
            "An activation receipt selects a Directive outside the applicable context.",
        ));
    }
    let mut directions = Vec::new();
    for candidate in candidates {
        let request_id = inputs
            .activation_receipts
            .get(candidate.id.as_str())
            .ok_or_else(workflow::unverified)?;
        let body = directives::read_approved(snapshot, &candidate, request_id, remaining)
            .map_err(|error| error.context_limit_ref("directive", &candidate.id))?;
        charge(&mut remaining, body.len())
            .map_err(|error| error.context_limit_ref("directive", &candidate.id))?;
        directions.push(json!({
            "kind": "directive", "trust": "approved-direction", "id": candidate.id,
            "revision": candidate.revision, "body": body, "issuer_ref": candidate.issuer_ref,
            "source_ref": candidate.source_ref, "approval_ref": candidate.approval_ref,
            "activation_request_id": request_id,
            "valid_from": candidate.valid_from, "expires_at": candidate.expires_at
        }));
    }
    let work = collect_work(snapshot, scope, Some(remaining)).map_err(|error| {
        if let Some(issue) = &scope.issue {
            error.context_limit_ref("issue", &issue.0)
        } else {
            error
        }
    })?;
    let mut data = json!({"identity": identity, "directives": directions});
    if let Some(work) = &work {
        let handoff = resume::read_required(snapshot, scope, work, remaining, &inputs.request)
            .map_err(|error| error.context_limit_ref("issue", &work.issue.id))?;
        charge(&mut remaining, work.inline_bytes)
            .map_err(|error| error.context_limit_ref("issue", &work.issue.id))?;
        data["issue"] = json!({
            "kind": "issue", "trust": "data", "record": work.issue,
            "blockers": {"prerequisite": work.prerequisite_blocked, "archived_ancestry": work.archived_ancestry, "hold": work.issue.hold_reason.is_some()},
            "claimed": work.claimed
        });
        if let Some(handoff) = &handoff {
            let source_kind = match handoff.kind {
                workflow::RecordKind::IssueUpdate => "issue_update",
                workflow::RecordKind::Artifact => "artifact",
            };
            charge(&mut remaining, handoff.inline_bytes)
                .map_err(|error| error.context_limit_ref(source_kind, &handoff.id))?;
            data["handoff"] = json!({
                "kind": "handoff", "trust": "data", "id": handoff.id, "revision": handoff.revision,
                "source_kind": source_kind,
                "body": handoff.body
            });
        }
        let mut artifacts = Vec::new();
        for entry in inputs.artifacts {
            let request = workflow::Request {
                input: Some(entry.evidence),
                ..inputs.request
            };
            let revision = resume::selected_artifact_revision(snapshot, work, entry.id, &request)?;
            if handoff.as_ref().is_some_and(|body| {
                body.kind == workflow::RecordKind::Artifact && body.id == entry.id
            }) {
                continue;
            }
            let body =
                resume::read_artifact_body(snapshot, scope, work, entry.id, &revision, remaining)
                    .map_err(|error| error.context_limit_ref("artifact", entry.id))?;
            charge(&mut remaining, body.len())
                .map_err(|error| error.context_limit_ref("artifact", entry.id))?;
            artifacts.push(json!({"kind": "artifact", "trust": "data", "id": entry.id, "revision": revision, "body": body}));
        }
        if !artifacts.is_empty() {
            data["artifacts"] = json!(artifacts);
        }
    } else if !inputs.artifacts.is_empty() {
        return Err(HiveError::new(
            ErrorCode::Unauthorized,
            "Explicit Artifact body disclosure requires exact Issue evidence.",
        ));
    }
    if let Some(project) = &scope.project {
        data["project"] = json!({
            "kind": "project", "trust": "data", "id": project.id, "revision": project.revision,
            "name": project.name, "status": project.status
        });
    }
    let object = data.as_object_mut().expect("context data object");
    let sections: Vec<Value> = [
        "identity",
        "directives",
        "issue",
        "project",
        "handoff",
        "artifacts",
    ]
    .into_iter()
    .filter_map(|section| {
        object
            .remove(section)
            .map(|content| json!({"section": section, "content": content}))
    })
    .collect();
    let data = json!({"inline_bytes": max - remaining, "sections": sections});
    let observed = inputs.request.observed_at.to_offset(UtcOffset::UTC);
    let observed_at = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        observed.year(),
        u8::from(observed.month()),
        observed.day(),
        observed.hour(),
        observed.minute(),
        observed.second(),
        observed.nanosecond()
    );
    let context = json!({
        "workspace_id": snapshot.workspace_id, "db_commit": snapshot.commit,
        "asset_commit": inputs.asset_commit, "policy_revision": inputs.policy_revision, "observed_at": observed_at,
        "scope": {
            "area_id": scope.ancestry.first().map(|area| &area.id),
            "issue_id": work.as_ref().map(|work| &work.issue.id),
            "project_id": scope.project.as_ref().map(|project| &project.id)
        }
    });
    if crate::output::context_json_bytes(&context, &data)? > MAX_JSON_BYTES {
        return Err(serialized_limit(inputs.role));
    }
    // Required sections capture the current applicable Directive set as well
    // as selected body revisions. The observation timestamp itself can advance.
    let required = format!("{:x}", Sha256::digest(data.to_string()));
    let binding = json!([
        "context-references-v1",
        snapshot.workspace_id,
        snapshot.commit,
        inputs.asset_commit,
        inputs.policy_revision,
        context["scope"],
        inputs.request.actor_ref,
        inputs.request.bot_id,
        inputs.request.invocation_id,
        inputs.role.policy_name(),
        max,
        required,
        inputs
            .artifacts
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        inputs.evidence_fingerprint,
    ]);
    let pages = references::collect(snapshot, scope, work.as_ref(), inputs.cursor, &binding)?;
    fit_output(
        context,
        data,
        pages,
        &binding,
        inputs.role,
        inputs.cursor.is_some(),
        MAX_JSON_BYTES,
    )
}

fn identity(
    snapshot: &mut Snapshot,
    inputs: &Inputs<'_>,
    remaining: &mut usize,
) -> Result<Value, HiveError> {
    let role = &inputs.role_body;
    if !crate::config_git::is_full_oid(inputs.asset_commit)
        || !valid_policy_revision(inputs.policy_revision)
        || role.policy_revision != inputs.policy_revision
        || role.role != inputs.role
        || !valid_body(role.body)
    {
        return Err(HiveError::new(
            ErrorCode::IncompatiblePolicy,
            "The required Role body does not match the selected policy revision.",
        ));
    }
    charge(remaining, role.body.len())
        .map_err(|error| error.context_limit_ref("role", role.role.policy_name()))?;
    let mut identity = json!({"role": {
        "kind": "role", "trust": "policy", "id": role.role.policy_name(),
        "revision": role.policy_revision, "body": role.body
    }});
    match (inputs.request.bot_id, &inputs.soul) {
        (None, None) => {}
        (Some(id), Some(soul))
            if matches!(inputs.role, ContextRole::Queen)
                && is_uuid_v7(id)
                && id == soul.bot_id
                && soul.workspace_id == snapshot.workspace_id
                && soul.db_commit == snapshot.commit
                && soul.asset_commit == inputs.asset_commit
                && crate::cli::is_slug(soul.bot_slug)
                && workflow::valid_ref(soul.registry_revision)
                && valid_body(soul.body) =>
        {
            let matches: Vec<bool> = snapshot.exec_rows(
                |commit| {
                    format!(
                        "SELECT retired_at IS NULL AND CAST(slug AS BINARY) = ?
                         AND CAST(revision AS BINARY) = ?
                     FROM bots AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
                    )
                },
                (soul.bot_slug, soul.registry_revision, id),
            )?;
            if matches.as_slice() != [true] {
                return Err(workflow::unverified());
            }
            charge(remaining, soul.body.len())
                .map_err(|error| error.context_limit_ref("soul", id))?;
            identity["soul"] = json!({
                "kind": "soul", "trust": "data", "id": id,
                "path": format!("assets/bots/{}/SOUL.md", soul.bot_slug),
                "revision": soul.asset_commit, "registry_revision": soul.registry_revision, "body": soul.body
            });
        }
        _ => return Err(workflow::unverified()),
    }
    Ok(identity)
}

fn valid_policy_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_body(body: &str) -> bool {
    !body.trim().is_empty() && !body.contains('\0')
}

fn charge(remaining: &mut usize, bytes: usize) -> Result<(), HiveError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or_else(|| limit("Required context text exceeds the remaining inline byte budget."))?;
    Ok(())
}

fn serialized_limit(role: ContextRole) -> HiveError {
    limit("Required context JSON exceeds the serialized response budget.")
        .context_limit_ref("role", role.policy_name())
}

fn fit_output(
    context: Value,
    mut data: Value,
    mut pages: Vec<references::Page>,
    binding: &Value,
    role: ContextRole,
    continuing: bool,
    max: usize,
) -> Result<DoltSuccess, HiveError> {
    loop {
        let mut omitted = json!({});
        let mut refs = json!({});
        for page in &pages {
            refs[page.group] = json!(page.items);
            if page.omitted != 0 {
                omitted[page.group] = json!({
                    "count": page.omitted,
                    "next_cursor": page.next_cursor(binding)
                });
            }
        }
        data["omitted"] = omitted;
        data["refs"] = refs;
        if crate::output::context_json_bytes(&context, &data)? <= max {
            return Ok(DoltSuccess { context, data });
        }
        let Some(page) = pages.iter_mut().rev().find(|page| !page.items.is_empty()) else {
            return Err(serialized_limit(role));
        };
        page.items.pop();
        page.omitted += 1;
        if continuing && page.items.is_empty() {
            return Err(
                limit("The response budget cannot fit the next context reference.")
                    .context_limit_ref("role", role.policy_name()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_full_envelope_and_omits_only_whole_optional_refs() {
        let context = json!({"scope": {"issue_id": "fixture"}});
        let required = json!({"identity": {"body": "é"}});
        let pages = || {
            vec![references::Page {
                group: "updates",
                offset: 0,
                items: vec![
                    json!({"id": "first"}),
                    json!({"id": "second", "meta": "x".repeat(250)}),
                ],
                omitted: 0,
            }]
        };
        let binding = json!("fixture");
        let role = ContextRole::Queen;
        let full = fit_output(
            context.clone(),
            required.clone(),
            pages(),
            &binding,
            role,
            false,
            MAX_JSON_BYTES,
        )
        .unwrap();
        let full_size = crate::output::context_json_bytes(&full.context, &full.data).unwrap();
        let fitted = fit_output(
            context.clone(),
            required,
            pages(),
            &binding,
            role,
            false,
            full_size - 1,
        )
        .unwrap();
        assert_eq!(fitted.data["identity"]["body"], "é");
        assert_eq!(fitted.data["refs"]["updates"], json!([{"id": "first"}]));
        assert_eq!(fitted.data["omitted"]["updates"]["count"], 1);
        let cursor = fitted.data["omitted"]["updates"]["next_cursor"]
            .as_str()
            .unwrap();
        assert!(cursor.starts_with("updates:1."));

        let escaped = json!({"required": "\u{0001}".repeat(250)});
        let error = fit_output(context, escaped, vec![], &binding, role, false, 1000)
            .err()
            .unwrap();
        assert!(matches!(error.code, ErrorCode::ContextLimit));
        assert_eq!(
            serde_json::to_value(error).unwrap()["details"],
            json!({"kind": "role", "id": "Queen"})
        );

        let mut budget = MAX_INLINE_BYTES;
        assert!(charge(&mut budget, MAX_INLINE_BYTES).is_ok());
        assert!(matches!(
            charge(&mut budget, 1).unwrap_err().code,
            ErrorCode::ContextLimit
        ));
    }
}
