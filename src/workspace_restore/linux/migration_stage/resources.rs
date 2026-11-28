use super::{Installed, Path, Value, io_error, new_uuid_v7, sql_text, unverified};
use crate::config_git::migration::{PreparedResources, ResourceHold};
use crate::error::HiveError;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use time::OffsetDateTime;

pub(super) const COUNTS: &str = "SELECT (SELECT COUNT(*) FROM resources) AS resources, \
    (SELECT COUNT(*) FROM area_resources) AS area_resources";

pub(super) struct StagedResources {
    resources: Vec<Value>,
    links: Vec<Value>,
}

impl StagedResources {
    pub(super) fn append(
        input: &mut impl Write,
        reviewed: Option<&PreparedResources<'_>>,
        now: OffsetDateTime,
    ) -> Result<Self, HiveError> {
        let mut resources = BTreeMap::new();
        let mut links = BTreeMap::new();
        for (_, prepared) in reviewed.into_iter().flat_map(PreparedResources::outcomes) {
            let Ok(prepared) = prepared else { continue };
            let resource = &prepared.resource;
            let link = &prepared.link;
            let kind = resource.kind.as_str();
            let revision = new_uuid_v7(now)?;
            writeln!(
                input,
                "INSERT INTO resources (id, kind, name, uri, revision) VALUES ({}, {}, {}, {}, {});",
                sql_text(resource.id),
                sql_text(kind),
                sql_text(resource.name),
                sql_text(resource.uri),
                sql_text(&revision),
            )
            .map_err(io_error)?;
            writeln!(
                input,
                "INSERT INTO area_resources (area_id, resource_id) VALUES ({}, {});",
                sql_text(link.area_id),
                sql_text(link.resource_id),
            )
            .map_err(io_error)?;
            resources.insert(
                resource.id,
                json!({
                    "id": resource.id,
                    "kind_sha256": digest(kind),
                    "name_sha256": digest(resource.name),
                    "uri_sha256": digest(resource.uri),
                    "revision": revision,
                }),
            );
            links.insert(
                (link.area_id, link.resource_id),
                json!({"area_id": link.area_id, "resource_id": link.resource_id}),
            );
        }
        Ok(Self {
            resources: resources.into_values().collect(),
            links: links.into_values().collect(),
        })
    }

    pub(super) fn verify_transaction_counts(&self, rows: &[Value]) -> Result<(), HiveError> {
        verify_counts(rows, self.resources.len(), self.links.len())
    }

    pub(super) fn verify_rollback(installed: &Installed<'_>, db: &Path) -> Result<(), HiveError> {
        verify_counts(&installed.query(db, COUNTS)?, 0, 0)
    }

    pub(super) fn verify_committed(
        &self,
        installed: &Installed<'_>,
        db: &Path,
        commit: &str,
    ) -> Result<(), HiveError> {
        if installed.query(
            db,
            &format!(
                "SELECT id, SHA2(kind, 256) AS kind_sha256, SHA2(name, 256) AS name_sha256, \
                 SHA2(uri, 256) AS uri_sha256, revision FROM resources AS OF '{commit}' ORDER BY id"
            ),
        )? != self.resources
        {
            return Err(unverified(
                "Committed Resources differ from the reviewed rows.",
            ));
        }
        if installed.query(
            db,
            &format!(
                "SELECT area_id, resource_id FROM area_resources AS OF '{commit}' ORDER BY area_id, resource_id"
            ),
        )? != self.links
        {
            return Err(unverified("Committed Area Resource links differ from the reviewed rows."));
        }
        Ok(())
    }

    pub(super) fn add_report(
        &self,
        report: &mut Value,
        reviewed: Option<&PreparedResources<'_>>,
        dry_run: bool,
    ) {
        let Some(reviewed) = reviewed else { return };
        let mut outcomes = Vec::new();
        let mut held = 0;
        let mut excluded = 0;
        for (id, outcome) in reviewed.outcomes() {
            let outcome = match outcome {
                Ok(prepared) => json!({
                    "status": "imported",
                    "resource_id": id,
                    "area_id": prepared.link.area_id,
                    "kind_sha256": digest(prepared.resource.kind.as_str()),
                    "name_sha256": digest(prepared.resource.name),
                    "uri_sha256": digest(prepared.resource.uri),
                }),
                Err(ResourceHold::ScopeExcluded) => {
                    excluded += 1;
                    json!({"status": "excluded", "resource_id": id, "reason_code": ResourceHold::ScopeExcluded})
                }
                Err(reason) => {
                    held += 1;
                    json!({"status": "held", "resource_id": id, "reason_code": reason})
                }
            };
            outcomes.push(outcome);
        }
        report["coverage"] = json!("scopes_and_resources");
        report["resource_bindings"] = json!({"outcomes": outcomes});
        report["counts"]["resources"] = json!({
            "inventoried": outcomes.len(),
            "imported": self.resources.len(),
            "held": held,
            "excluded_non_record": excluded,
        });
        report["checks"]["target_resource_rows"] =
            json!(if dry_run { 0 } else { self.resources.len() });
        report["checks"]["target_area_resource_rows"] =
            json!(if dry_run { 0 } else { self.links.len() });
    }
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn verify_counts(rows: &[Value], resources: usize, links: usize) -> Result<(), HiveError> {
    if rows != [json!({"resources": resources, "area_resources": links})] {
        return Err(unverified(
            "Migration Resource or Area Resource link counts differ.",
        ));
    }
    Ok(())
}
