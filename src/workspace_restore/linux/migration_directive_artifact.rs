use std::collections::{BTreeMap, VecDeque};
use std::io::Write;

use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{
    Installed, Path, Value, io_error, json, new_uuid_v7, sql_text, tasks::TaskImport, unverified,
};
use crate::config_git::migration::prepared_records::{
    ArtifactKind, DirectiveStatus, Fields, Intake, PreparedRecord,
};
use crate::error::HiveError;

#[derive(Default)]
pub(super) struct Expected {
    directives: BTreeMap<String, Value>,
    artifacts: BTreeMap<String, Value>,
    issue_links: BTreeMap<String, String>,
    area_links: BTreeMap<String, String>,
}

impl Expected {
    pub(super) fn counts(&self) -> (usize, usize) {
        (self.directives.len(), self.artifacts.len())
    }

    pub(super) fn link_count(&self) -> usize {
        self.issue_links.len() + self.area_links.len()
    }

    pub(super) fn transaction_checks(&self) -> impl Iterator<Item = (String, Vec<Value>)> + use<> {
        std::iter::once(self.count_check(""))
            .chain(self.checks("").filter(|(_, expected)| !expected.is_empty()))
    }

    pub(super) fn verify_empty(installed: &Installed<'_>, db: &Path) -> Result<(), HiveError> {
        let (query, expected) = Self::default().count_check("");
        if installed.query(db, &query)? != expected {
            return Err(unverified(
                "Directive or Artifact rows remain after rollback.",
            ));
        }
        Ok(())
    }

    pub(super) fn verify_committed(
        &self,
        installed: &Installed<'_>,
        db: &Path,
        commit: &str,
    ) -> Result<(), HiveError> {
        let snapshot = format!(" AS OF '{commit}'");
        for (query, expected) in
            std::iter::once(self.count_check(&snapshot)).chain(self.checks(&snapshot))
        {
            if installed.query(db, &query)? != expected {
                return Err(unverified(
                    "Committed Directive or Artifact rows differ from reviewed content.",
                ));
            }
        }
        Ok(())
    }

    fn count_check(&self, snapshot: &str) -> (String, Vec<Value>) {
        let (directives, artifacts) = self.counts();
        (format!("SELECT (SELECT COUNT(*) FROM directives{snapshot}) AS directives,
            (SELECT COUNT(*) FROM artifacts{snapshot}) AS artifacts,
            ((SELECT COUNT(*) FROM artifact_areas{snapshot}) + (SELECT COUNT(*) FROM artifact_projects{snapshot}) +
             (SELECT COUNT(*) FROM artifact_issues{snapshot}) + (SELECT COUNT(*) FROM artifact_resources{snapshot})) AS artifact_links"),
            vec![json!({"directives": directives, "artifacts": artifacts, "artifact_links": self.link_count()})])
    }

    fn checks(&self, snapshot: &str) -> impl Iterator<Item = (String, Vec<Value>)> + use<> {
        [
            (format!("SELECT id, status, scope_kind, area_id, project_id,
                IF(include_descendants, 1, 0) AS include_descendants, SHA2(title, 256) AS title_sha256,
                SHA2(body, 256) AS body_sha256, SHA2(issuer_ref, 256) AS issuer_ref_sha256,
                SHA2(source_ref, 256) AS source_ref_sha256, SHA2(approval_ref, 256) AS approval_ref_sha256,
                DATE_FORMAT(valid_from, '%Y-%m-%dT%H:%i:%s.%fZ') AS valid_from,
                DATE_FORMAT(expires_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS expires_at, supersedes_id, revision,
                DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS created_at,
                DATE_FORMAT(updated_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS updated_at
                FROM directives{snapshot} ORDER BY id"), self.directives.values().cloned().collect()),
            (format!("SELECT id, kind, SHA2(title, 256) AS title_sha256, SHA2(body, 256) AS body_sha256,
                SHA2(author_ref, 256) AS author_ref_sha256, SHA2(source_ref, 256) AS source_ref_sha256, revision,
                DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS created_at,
                DATE_FORMAT(substantive_updated_at, '%Y-%m-%dT%H:%i:%s.%fZ') AS substantive_updated_at,
                archived_at, archive_reason FROM artifacts{snapshot} ORDER BY id"), self.artifacts.values().cloned().collect()),
            (format!("SELECT artifact_id, issue_id FROM artifact_issues{snapshot} ORDER BY artifact_id, issue_id"),
                self.issue_links.iter().map(|(artifact, issue)| json!({"artifact_id": artifact, "issue_id": issue})).collect()),
            (format!("SELECT artifact_id, area_id FROM artifact_areas{snapshot} ORDER BY artifact_id, area_id"),
                self.area_links.iter().map(|(artifact, area)| json!({"artifact_id": artifact, "area_id": area})).collect()),
        ].into_iter()
    }
}

pub(super) fn append(
    sql: &mut impl Write,
    records: &[PreparedRecord],
    tasks: Option<&TaskImport<'_>>,
    now: OffsetDateTime,
) -> Result<Expected, HiveError> {
    let mut expected = Expected::default();
    for record in in_insert_order(records)? {
        let revision = new_uuid_v7(now)?;
        let mut row = json!({"id": record.id, "title_sha256": digest(&record.content.title),
            "body_sha256": digest(&record.content.body), "revision": revision});
        let id = sql_text(&record.id);
        let revision = sql_text(&revision);
        let title = sql_text(&record.content.title);
        let body = sql_text(&record.content.body);
        match &record.fields {
            Fields::Directive(fields) => {
                let status = directive_status(fields.status);
                row["status"] = json!(status);
                row["scope_kind"] = json!("area");
                row["area_id"] = json!(fields.area_id);
                row["include_descendants"] = json!(0);
                row["issuer_ref_sha256"] = json!(digest(&fields.issuer_ref));
                row["source_ref_sha256"] = json!(digest(&fields.source_ref));
                row["created_at"] = json!(report_time(&fields.created_at));
                row["updated_at"] = json!(report_time(&fields.updated_at));
                if let Some(value) = &fields.approval_ref {
                    row["approval_ref_sha256"] = json!(digest(value));
                }
                if let Some(value) = &fields.valid_from {
                    row["valid_from"] = json!(report_time(value));
                }
                if let Some(value) = &fields.expires_at {
                    row["expires_at"] = json!(report_time(value));
                }
                if let Some(value) = &fields.supersedes_id {
                    row["supersedes_id"] = json!(value);
                }
                writeln!(sql, "INSERT INTO directives (id, scope_kind, area_id, include_descendants, title, body,
                    status, issuer_ref, source_ref, approval_ref, valid_from, expires_at, supersedes_id,
                    revision, created_at, updated_at) VALUES ({id}, 'area', {}, FALSE, {title}, {body},
                    {}, {}, {}, {}, {}, {}, {}, {revision}, {}, {});",
                    sql_text(&fields.area_id), sql_text(status), sql_text(&fields.issuer_ref), sql_text(&fields.source_ref),
                    nullable(fields.approval_ref.as_deref(), sql_text), nullable(fields.valid_from.as_deref(), sql_time),
                    nullable(fields.expires_at.as_deref(), sql_time), nullable(fields.supersedes_id.as_deref(), sql_text),
                    sql_time(&fields.created_at), sql_time(&fields.updated_at)).map_err(io_error)?;
                expected.directives.insert(record.id.clone(), row);
            }
            Fields::Artifact(fields) => {
                let kind = artifact_kind(fields.kind);
                row["kind"] = json!(kind);
                row["author_ref_sha256"] = json!(digest(&fields.author_ref));
                row["source_ref_sha256"] = json!(digest(&fields.source_ref));
                row["created_at"] = json!(report_time(&fields.created_at));
                row["substantive_updated_at"] = json!(report_time(&fields.substantive_updated_at));
                writeln!(sql, "INSERT INTO artifacts (id, kind, title, body, author_ref, source_ref, revision,
                    created_at, substantive_updated_at) VALUES ({id}, {}, {title}, {body}, {}, {}, {revision}, {}, {});",
                    sql_text(kind), sql_text(&fields.author_ref), sql_text(&fields.source_ref),
                    sql_time(&fields.created_at), sql_time(&fields.substantive_updated_at)).map_err(io_error)?;
                if let Some(work) = &fields.work {
                    let link = &work.issue_link;
                    let issue = tasks.and_then(|tasks| tasks.issue_id(link)).ok_or_else(|| {
                        unverified("A prepared work-scoped Artifact has no eligible Issue in this transaction.")
                    })?;
                    writeln!(
                        sql,
                        "INSERT INTO artifact_issues (artifact_id, issue_id) VALUES ({id}, {});",
                        sql_text(issue)
                    )
                    .map_err(io_error)?;
                    expected
                        .issue_links
                        .insert(record.id.clone(), issue.to_owned());
                    if let Some(area_id) = &link.joined_area_id {
                        writeln!(
                            sql,
                            "INSERT INTO artifact_areas (artifact_id, area_id) VALUES ({id}, {});",
                            sql_text(area_id)
                        )
                        .map_err(io_error)?;
                        expected
                            .area_links
                            .insert(record.id.clone(), area_id.clone());
                    }
                }
                expected.artifacts.insert(record.id.clone(), row);
            }
        }
    }
    Ok(expected)
}

fn in_insert_order(records: &[PreparedRecord]) -> Result<Vec<&PreparedRecord>, HiveError> {
    let mut roots = VecDeque::new();
    let mut replacements = BTreeMap::<&str, &PreparedRecord>::new();
    let mut artifacts = Vec::new();
    for record in records {
        match &record.fields {
            Fields::Directive(fields) => {
                if let Some(predecessor) = &fields.supersedes_id {
                    if replacements.insert(predecessor, record).is_some() {
                        return Err(unverified("Prepared Directive supersession is ambiguous."));
                    }
                } else {
                    roots.push_back(record);
                }
            }
            Fields::Artifact(_) => artifacts.push(record),
        }
    }
    let mut ordered = Vec::with_capacity(records.len());
    while let Some(directive) = roots.pop_front() {
        ordered.push(directive);
        if let Some(replacement) = replacements.remove(directive.id.as_str()) {
            roots.push_back(replacement);
        }
    }
    if !replacements.is_empty() {
        return Err(unverified(
            "Prepared Directive supersession cannot be ordered.",
        ));
    }
    ordered.extend(artifacts);
    Ok(ordered)
}

pub(super) fn add_report(report: &mut Value, mut intake: Intake, expected: Option<&Expected>) {
    if let Some(counts) = intake.report["counts"].as_object() {
        for (surface, counts) in counts {
            report["counts"][surface] = json!({"inventoried": counts["inventoried"],
                "imported": counts["prepared"].as_u64().unwrap_or(0), "held": counts["held"], "excluded_non_record": counts["excluded"]});
        }
    }
    if let Some(records) = intake.report["records"].as_array_mut() {
        for record in records {
            let record = record.as_object_mut().expect("intake row object");
            for (locator, digest_field) in [
                ("source_path", "source_path_sha256"),
                ("parent_task_path", "parent_task_path_sha256"),
            ] {
                if let Some(path) = record
                    .remove(locator)
                    .and_then(|value| value.as_str().map(digest))
                {
                    record.insert(digest_field.to_owned(), json!(path));
                }
            }
            if let Some(issue) = record["outcome"]["target_id"]
                .as_str()
                .and_then(|id| expected?.issue_links.get(id))
            {
                let link = record.get_mut("outcome").expect("prepared outcome")["provenance"]["issue_link"]
                    .as_object_mut().expect("prepared Artifact link");
                if let Some(key) = link.remove("legacy_key") {
                    let scope = key["scope"].as_str().expect("prepared Task scope");
                    let work = key["work_id"].as_str().expect("prepared Task work ID");
                    link.insert(
                        "legacy_key_sha256".to_owned(),
                        json!(digest(&format!("{scope}\0{work}"))),
                    );
                }
                link.insert("issue_id".to_owned(), json!(issue));
            }
            let section = match record["outcome"]["status"].as_str() {
                Some("prepared") => "id_map",
                Some("held") => "holds",
                _ => "exclusions",
            };
            report[section]
                .as_array_mut()
                .expect("migration evidence array")
                .push(json!(record));
        }
    }
}

fn nullable(value: Option<&str>, encode: fn(&str) -> String) -> String {
    value.map_or_else(|| "NULL".to_owned(), encode)
}

fn report_time(value: &str) -> String {
    let value = value.trim_end_matches('Z').replace('t', "T");
    let (seconds, fraction) = value.split_once('.').unwrap_or((&value, ""));
    format!("{seconds}.{fraction:0<6}Z")
}

fn sql_time(value: &str) -> String {
    sql_text(&report_time(value).trim_end_matches('Z').replace('T', " "))
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn directive_status(value: DirectiveStatus) -> &'static str {
    match value {
        DirectiveStatus::Proposed => "proposed",
        DirectiveStatus::Active => "active",
        DirectiveStatus::Withdrawn => "withdrawn",
        DirectiveStatus::Superseded => "superseded",
    }
}

fn artifact_kind(value: ArtifactKind) -> &'static str {
    match value {
        ArtifactKind::Report => "report",
        ArtifactKind::Research => "research",
        ArtifactKind::Review => "review",
        ArtifactKind::Handoff => "handoff",
        ArtifactKind::Message => "message",
        ArtifactKind::Checkpoint => "checkpoint",
        ArtifactKind::WorkNote => "work-note",
        ArtifactKind::Manifest => "manifest",
    }
}
