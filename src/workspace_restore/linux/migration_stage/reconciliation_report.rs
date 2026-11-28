use std::collections::BTreeMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{directive_artifact, unverified};
use crate::config_git::migration::{PreparedResources, ReviewedScopes};
use crate::error::HiveError;

pub(super) fn complete(
    report: &mut Value,
    reviewed: &ReviewedScopes<'_>,
    soul_blobs: &BTreeMap<String, String>,
    dry_run: bool,
) -> Result<(), HiveError> {
    let mut inventory_only = Vec::new();
    if report["counts"].get("tasks").is_none() {
        inventory_only.push("tasks");
        add_task_ledger(report, reviewed.unselected_task_ledger()?);
    }
    if report["counts"].get("directives").is_none() {
        inventory_only.extend([
            "directives",
            "artifacts_standalone",
            "artifacts_work_scoped",
        ]);
        directive_artifact::add_report(
            report,
            reviewed.unselected_directive_artifact_inventory()?,
            None,
        );
    }
    if report.get("queen_preparation").is_none() {
        inventory_only.push("bots_soul");
    }
    let mut assets = reviewed.asset_reconciliation()?;
    report["counts"]
        .as_object_mut()
        .expect("migration counts")
        .append(assets["counts"].as_object_mut().expect("Bot asset counts"));
    append_ledger(report, &mut assets);

    add_soul_ids(report, soul_blobs)?;
    if let Some(resources) = &reviewed.resources {
        add_resources(report, resources);
    }
    add_ledger_aliases(report);

    let hive_yml = &report["source"]["hive_yml"];
    let mode = hive_yml["mode"].as_str().unwrap_or("absent");
    let version = hive_yml["version"].clone();
    report["source"]["hive_yml_mode"] = json!(mode);
    report["source"]["hive_yml_version"] = version;
    // The pinned v1 workspace config carries no logical workspace label.
    report["source"]["workspace_label"] = Value::Null;

    report["reconciliation"] = json!({
        // Existing imported counts include rows verified inside the rolled-back transaction.
        "counts_and_id_map_basis": if dry_run { "rolled_back_transaction" } else { "confirmed_commit" },
        "asset_commit_basis": if dry_run { "verified_disposable_commit" } else { "verified_published_commit" },
        "inventory_only_surfaces": inventory_only,
        "checks": {
            "transaction_row_checks": "passed",
            "committed_schema_constraints": if dry_run { "not_run" } else { "passed" },
            "committed_completion_obligations": if dry_run { "not_run" } else { "passed" },
            "issue_counter_and_zero_claims": "verified",
            "artifact_and_issue_retention_fields": "verified_for_imported_rows",
            "source_snapshot": "verified",
        }
    });
    Ok(())
}

pub(super) fn add_task_ledger(report: &mut Value, mut ledger: Value) {
    report["counts"]["tasks"] = ledger["counts"].take();
    append_ledger(report, &mut ledger);
}

fn append_ledger(report: &mut Value, ledger: &mut Value) {
    for key in ["holds", "exclusions"] {
        report[key]
            .as_array_mut()
            .expect("migration ledger")
            .append(
                ledger[key]
                    .as_array_mut()
                    .expect("incoming migration ledger"),
            );
    }
}

fn add_soul_ids(
    report: &mut Value,
    soul_blobs: &BTreeMap<String, String>,
) -> Result<(), HiveError> {
    let mut ids = Vec::new();
    for row in report["queen_preparation"].as_array().into_iter().flatten() {
        if row["status"] != "prepared" {
            continue;
        }
        let slug = row["slug"].as_str().expect("prepared Queen slug");
        let path = format!("bots/{slug}/SOUL.md");
        let blob = soul_blobs
            .get(&path)
            .ok_or_else(|| unverified("The verified asset snapshot is missing a prepared Soul."))?;
        ids.push(json!({
            "kind": "bot_soul", "surface": "bots_soul",
            "legacy_key_sha256": digest(slug),
            "source_path_sha256": digest(&path),
            "source_git_blob_sha": row["source_git_blob_sha"],
            "source_raw_sha256": row["source_raw_sha256"],
            "target_body_sha256": row["source_raw_sha256"],
            "asset_git_blob_sha": blob,
            "bot_id": row["bot_id"], "target_id": row["bot_id"], "slug": slug,
        }));
    }
    report["id_map"]
        .as_array_mut()
        .expect("migration ID map")
        .extend(ids);
    Ok(())
}

fn add_resources(report: &mut Value, resources: &PreparedResources<'_>) {
    let mut ids_by_area = BTreeMap::<String, Vec<String>>::new();
    let mut ledger = json!({"holds": [], "exclusions": []});
    let mut ids = Vec::new();
    for row in resources.reconciliation_entries() {
        let section = match row["status"].as_str() {
            Some("imported") => {
                let area = row["area_id"].as_str().expect("verified Area ID");
                let resource = row["resource_id"].as_str().expect("verified Resource ID");
                ids_by_area
                    .entry(area.to_owned())
                    .or_default()
                    .push(resource.to_owned());
                ids.push(row);
                continue;
            }
            Some("held") => "holds",
            _ => "exclusions",
        };
        ledger[section]
            .as_array_mut()
            .expect("Resource ledger")
            .push(row);
    }
    for row in report["id_map"].as_array_mut().expect("migration ID map") {
        if row.get("scope_sha256").is_some()
            && let Some(area) = row["area_id"].as_str()
            && let Some(mut resources) = ids_by_area.remove(area)
        {
            resources.sort_unstable();
            row["resource_ids"] = json!(resources);
        }
    }
    report["id_map"]
        .as_array_mut()
        .expect("migration ID map")
        .extend(ids);
    append_ledger(report, &mut ledger);
}

fn add_ledger_aliases(report: &mut Value) {
    let source_commit = report["source"]["source_commit"].clone();
    for section in ["id_map", "holds", "exclusions"] {
        for row in report[section].as_array_mut().expect("migration ledger") {
            let (kind, surface, target_field) = match row["surface"].as_str() {
                Some("directives") => ("directive", "directives", "directive_id"),
                Some("artifacts_standalone") => ("artifact", "artifacts_standalone", "artifact_id"),
                Some("artifacts_work_scoped") => {
                    ("artifact", "artifacts_work_scoped", "artifact_id")
                }
                Some("bots_soul") => ("bot_soul", "bots_soul", "bot_id"),
                Some("bot_memories_historical") => ("bot_memory", "bot_memories_historical", ""),
                Some("navigation_only") => ("navigation", "navigation_only", ""),
                Some("resources") => ("resource", "resources", "resource_id"),
                _ if row.get("lifecycle").is_some() => ("task", "tasks", "issue_id"),
                _ => ("scope", "scopes", "area_id"),
            };
            row["kind"] = json!(kind);
            row["surface"] = json!(surface);
            if kind == "scope" {
                row["legacy_key_sha256"] = row["scope_sha256"].clone();
            }
            if section == "id_map" {
                if let Some(target) = row["outcome"].get("target_id").cloned() {
                    row[target_field] = target;
                    row["target_body_sha256"] = row["outcome"]["target_body_sha256"].clone();
                }
                row["target_id"] = row[target_field].clone();
                continue;
            }
            row["source_commit"] = source_commit.clone();
            if section == "holds" {
                let reasons = row
                    .get("reason_codes")
                    .or_else(|| row["outcome"].get("reason_codes"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| vec![row["reason_code"].clone()]);
                row["reason_code"] = reasons[0].clone();
                row["reconciliation_actions"] = json!(reasons.iter().map(|reason| {
                    json!({"reason_code": reason, "action": action(reason.as_str().expect("typed hold reason"))})
                }).collect::<Vec<_>>());
                row["reason_codes"] = json!(reasons);
            }
        }
    }
}

fn action(reason: &str) -> &'static str {
    match reason {
        "ambiguous_scope_or_collision"
        | "unsplit_legacy_scope"
        | "scope_held"
        | "area_mismatch" => "Resolve the legacy scope and its unique target Area mapping.",
        "execution_needs_reconciliation" => {
            "Reconcile in-flight execution and its historical Claim before retrying."
        }
        "missing_typed_submission_evidence" => {
            "Review the original submission and recover typed lifecycle evidence."
        }
        "unresolved_task_relations" | "unresolved_completion_obligation" => {
            "Resolve Task relationship endpoints and verify historical completion obligations."
        }
        "unverified_acceptance_criteria"
        | "unverified_task_labels"
        | "task_review_binding_mismatch"
        | "conflicting_task_review" => {
            "Correct the Task review and bind it to the exact committed entrypoint."
        }
        "unresolved_parent_task" | "missing_parent_task_entrypoint" | "parent_task_quarantined" => {
            "Reconcile the parent Task and review the Artifact link."
        }
        "task_label_collation_collision" => {
            "Reconcile colliding label spellings under the target column collation."
        }
        "missing_directive_approval" | "unverified_directive_validity" => {
            "Review historical Directive approval and validity evidence."
        }
        "unresolved_supersession_graph" | "invalid_supersession_chain" => {
            "Resolve the exact Directive supersession chain and review its endpoints."
        }
        "unclassified_artifact_kind" | "invalid_artifact_status" => {
            "Review the committed Artifact classification and supported target metadata."
        }
        "unresolved_artifact_links"
        | "unresolved_artifact_bundle_member"
        | "unresolved_binary_or_external_owner" => {
            "Resolve Artifact ownership and review the required links or Resource mapping."
        }
        "unverified_artifact_author"
        | "unverified_artifact_retention"
        | "incomplete_historical_provenance" => {
            "Recover and review historical provenance from the verified source."
        }
        "invalid_soul_or_slug" | "unverified_queen_preparation" => {
            "Review the committed Soul identity, exact blob and target Bot binding."
        }
        "curated_memory_deferred" => {
            "Keep historical Memory in the source for explicit operator review."
        }
        "unverified_resource_evidence" | "resource_collision" => {
            "Review the committed Resource evidence and its unique Area binding."
        }
        _ => "Review the cited source entry at the verified commit and resolve the recorded hold.",
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value))
}
