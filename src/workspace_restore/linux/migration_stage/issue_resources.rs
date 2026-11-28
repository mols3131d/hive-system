use std::collections::BTreeSet;
use std::io::Write;

use serde_json::{Value, json};

use super::{TaskImport, io_error, sql_text, unverified};
use crate::config_git::migration::IssueResourceOutcome;
use crate::error::HiveError;

pub(super) const COUNT: &str = "SELECT COUNT(*) AS issue_resources FROM issue_resources";

pub(super) struct IssueResources<'a> {
    pairs: BTreeSet<(&'a str, &'a str)>,
    rows: Vec<Value>,
    count: [Value; 1],
    outcomes: Option<Vec<Value>>,
}

impl<'a> IssueResources<'a> {
    pub(super) fn new(
        outcomes: Option<&'a [IssueResourceOutcome<'_>]>,
        tasks: Option<&'a TaskImport<'_>>,
    ) -> Result<Self, HiveError> {
        let mut pairs = BTreeSet::new();
        let outcomes = outcomes
            .map(|outcomes| {
                outcomes
                    .iter()
                    .map(|outcome| {
                        let mut report = outcome.report();
                        if outcome.hold.is_none() {
                            let issue_id = tasks
                                .and_then(|tasks| tasks.issue_id_by_key(outcome.key))
                                .ok_or_else(|| {
                                    unverified(
                                        "A prepared Issue Resource link has no staged Issue.",
                                    )
                                })?;
                            pairs.insert((issue_id, outcome.resource_id));
                            report["status"] = json!("imported");
                            report["issue_id"] = json!(issue_id);
                        }
                        Ok(report)
                    })
                    .collect::<Result<Vec<_>, HiveError>>()
            })
            .transpose()?;
        let rows = pairs
            .iter()
            .map(|(issue, resource)| json!({"issue_id": issue, "resource_id": resource}))
            .collect();
        Ok(Self {
            count: count(pairs.len()),
            pairs,
            rows,
            outcomes,
        })
    }

    pub(super) fn append_sql(&self, input: &mut impl Write) -> Result<(), HiveError> {
        for (issue, resource) in &self.pairs {
            writeln!(
                input,
                "INSERT INTO issue_resources (issue_id, resource_id) VALUES ({}, {});",
                sql_text(issue),
                sql_text(resource),
            )
            .map_err(io_error)?;
        }
        Ok(())
    }

    pub(super) fn checks(&self, commit: Option<&str>) -> Vec<(String, &[Value])> {
        let snapshot = commit.map_or_else(String::new, |hash| format!(" AS OF '{hash}'"));
        let mut checks = vec![(format!("{COUNT}{snapshot}"), self.count.as_slice())];
        if !self.rows.is_empty() {
            checks.push((
                format!("SELECT issue_id, resource_id FROM issue_resources{snapshot} ORDER BY issue_id, resource_id"),
                self.rows.as_slice(),
            ));
        }
        checks
    }

    pub(super) fn add_report(&self, report: &mut Value, dry_run: bool) {
        if let Some(outcomes) = &self.outcomes {
            report["issue_resource_bindings"] = json!(outcomes);
            report["counts"]["issue_resources"] = json!({
                "inventoried": outcomes.len(),
                "imported": self.pairs.len(),
                "held": outcomes.len() - self.pairs.len(),
            });
            report["checks"]["target_issue_resource_rows"] =
                json!(if dry_run { 0 } else { self.pairs.len() });
        }
    }
}

pub(super) fn count(rows: usize) -> [Value; 1] {
    [json!({"issue_resources": rows})]
}
