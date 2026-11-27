#[cfg(any(test, target_os = "linux"))]
#[path = "context.rs"]
pub(super) mod context;

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::{ErrorCode, HiveError};

use super::records::{Diagnostics, IssueStatus, is_issue_type};
use super::{DoltFailure, DoltSuccess, IssueShowArgs, Snapshot, is_uuid_v7};

const RELATION_REF_LIMIT: usize = 25;
const MAX_INLINE_BYTES: usize = 65_536;
const MAX_RESPONSE_BYTES: usize = 524_288;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum IssueSelector {
    Uuid(String),
    Number(u64),
}

type IssueMetaRow = (
    String,
    u64,
    String,
    Option<String>,
    String,
    String,
    Option<i8>,
    String,
    String,
    String,
    u64,
);

type IssueTextRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Serialize)]
struct IssueDetail {
    id: String,
    number: u64,
    area_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_criteria: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<i8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hold_reason: Option<String>,
    revision: String,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    closed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    closed_by_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution_note: Option<String>,
}

impl IssueDetail {
    fn inline_bytes(&self) -> usize {
        [
            Some(self.title.as_str()),
            self.body.as_deref(),
            self.acceptance_criteria.as_deref(),
            self.hold_reason.as_deref(),
            self.resolution_note.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::len)
        .sum()
    }
}

#[derive(Serialize)]
struct BoundedIds {
    items: Vec<String>,
    truncated: bool,
}

#[derive(Serialize)]
struct IssueShowData {
    issue: IssueDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    claim_id: Option<String>,
    children: BoundedIds,
    prerequisites: BoundedIds,
    dependents: BoundedIds,
    projects: BoundedIds,
    labels: BoundedIds,
    resources: BoundedIds,
    artifacts: BoundedIds,
    updates: BoundedIds,
}

pub(crate) fn show(args: &IssueShowArgs) -> Result<DoltSuccess, Box<DoltFailure>> {
    let selector = parse_selector(&args.issue).map_err(DoltFailure::without_context)?;
    let (mut snapshot, workspace_id) =
        Snapshot::open(&args.target).map_err(DoltFailure::without_context)?;
    let context = json!({
        "workspace_id": workspace_id,
        "db_commit": snapshot.commit,
        "scope": "issue",
    });

    match show_in_snapshot(&mut snapshot, &selector) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn show_in_snapshot(snapshot: &mut Snapshot, selector: &IssueSelector) -> Result<Value, HiveError> {
    let meta = load_issue_meta(snapshot, selector)?;
    let issue = load_issue_detail(snapshot, meta, MAX_INLINE_BYTES)?;
    let mut diagnostics = Diagnostics::default();
    validate_issue(&issue, &mut diagnostics);

    let claim_id = load_claim_id(snapshot, &issue, &mut diagnostics)?;
    let children = load_child_ids(snapshot, &issue.id, &mut diagnostics)?;
    let prerequisites = load_relation_ids(
        snapshot,
        "issue_dependencies",
        "prerequisite_id",
        "issue_id",
        "prerequisite_id ASC",
        &issue.id,
        &mut diagnostics,
    )?;
    let dependents = load_relation_ids(
        snapshot,
        "issue_dependencies",
        "issue_id",
        "prerequisite_id",
        "issue_id ASC",
        &issue.id,
        &mut diagnostics,
    )?;
    let projects = load_relation_ids(
        snapshot,
        "project_issues",
        "project_id",
        "issue_id",
        "project_id ASC",
        &issue.id,
        &mut diagnostics,
    )?;
    let labels = load_relation_ids(
        snapshot,
        "issue_labels",
        "label_id",
        "issue_id",
        "label_id ASC",
        &issue.id,
        &mut diagnostics,
    )?;
    let resources = load_relation_ids(
        snapshot,
        "issue_resources",
        "resource_id",
        "issue_id",
        "resource_id ASC",
        &issue.id,
        &mut diagnostics,
    )?;
    let artifacts = load_relation_ids(
        snapshot,
        "artifact_issues",
        "artifact_id",
        "issue_id",
        "artifact_id ASC",
        &issue.id,
        &mut diagnostics,
    )?;
    let updates = load_relation_ids(
        snapshot,
        "issue_updates",
        "id",
        "issue_id",
        "created_at DESC, id ASC",
        &issue.id,
        &mut diagnostics,
    )?;

    if !diagnostics.items.is_empty() {
        return Err(HiveError::record_violations(
            diagnostics.items,
            diagnostics.truncated,
        ));
    }

    let detail = IssueShowData {
        issue,
        claim_id,
        children,
        prerequisites,
        dependents,
        projects,
        labels,
        resources,
        artifacts,
        updates,
    };

    let encoded = serde_json::to_vec(&detail).map_err(|_| {
        HiveError::new(
            ErrorCode::InternalError,
            "Failed to encode Issue details as JSON.",
        )
    })?;
    if encoded.len() > MAX_RESPONSE_BYTES {
        return Err(HiveError::new(
            ErrorCode::ContextLimit,
            format!(
                "The selected Issue response exceeds the maximum payload size of {MAX_RESPONSE_BYTES} bytes."
            ),
        ));
    }
    serde_json::to_value(detail).map_err(|_| {
        HiveError::new(
            ErrorCode::InternalError,
            "Failed to build Issue response value.",
        )
    })
}

fn load_issue_detail(
    snapshot: &mut Snapshot,
    meta: IssueMetaRow,
    byte_limit: usize,
) -> Result<IssueDetail, HiveError> {
    let (
        id,
        number,
        area_id,
        parent_id,
        kind,
        status,
        priority,
        revision,
        created_at,
        updated_at,
        inline_bytes,
    ) = meta;

    if inline_bytes > byte_limit as u64 {
        return Err(HiveError::new(
            ErrorCode::ContextLimit,
            format!(
                "The selected Issue exceeds the maximum inline detail size of {byte_limit} bytes."
            ),
        ));
    }

    let (title, body, acceptance_criteria, hold_reason, closed_at, closed_by_ref, resolution_note) =
        load_issue_text(snapshot, &id)?;
    let issue = IssueDetail {
        id,
        number,
        area_id,
        parent_id,
        title,
        body,
        acceptance_criteria,
        kind,
        status,
        priority,
        hold_reason,
        revision,
        created_at,
        updated_at,
        closed_at,
        closed_by_ref,
        resolution_note,
    };

    if issue.inline_bytes() > byte_limit {
        return Err(HiveError::new(
            ErrorCode::ContextLimit,
            format!(
                "The selected Issue exceeds the maximum inline detail size of {byte_limit} bytes."
            ),
        ));
    }

    Ok(issue)
}

pub(super) fn parse_selector(value: &str) -> Result<IssueSelector, HiveError> {
    if is_uuid_v7(value) {
        return Ok(IssueSelector::Uuid(value.to_owned()));
    }
    if let Some(digits) = value.strip_prefix('#')
        && !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(number) = digits.parse::<u64>()
        && number > 0
    {
        return Ok(IssueSelector::Number(number));
    }
    Err(HiveError::new(
        ErrorCode::InvalidArgument,
        "Supply a canonical lowercase UUIDv7 or positive decimal #number Issue selector.",
    ))
}

fn load_issue_meta(
    snapshot: &mut Snapshot,
    selector: &IssueSelector,
) -> Result<IssueMetaRow, HiveError> {
    let columns = "id, number, area_id, parent_id, type, status, priority, revision, \
        DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%s.%fZ'), \
        DATE_FORMAT(updated_at, '%Y-%m-%dT%H:%i:%s.%fZ'), \
        COALESCE(OCTET_LENGTH(title), 0) + COALESCE(OCTET_LENGTH(body), 0) \
        + COALESCE(OCTET_LENGTH(acceptance_criteria), 0) \
        + COALESCE(OCTET_LENGTH(hold_reason), 0) \
        + COALESCE(OCTET_LENGTH(resolution_note), 0)";
    let mut rows: Vec<IssueMetaRow> = match selector {
        IssueSelector::Uuid(id) => snapshot.exec_rows(
            |commit| format!("SELECT {columns} FROM issues AS OF '{commit}' WHERE id = ? LIMIT 2"),
            (id,),
        )?,
        IssueSelector::Number(number) => snapshot.exec_rows(
            |commit| {
                format!("SELECT {columns} FROM issues AS OF '{commit}' WHERE number = ? LIMIT 2")
            },
            (*number,),
        )?,
    };
    if rows.len() > 1 {
        return Err(HiveError::new(
            ErrorCode::SchemaIncompatible,
            "Dolt returned multiple rows for a unique Issue selector.",
        ));
    }
    let Some(row) = rows.pop() else {
        return Err(not_found());
    };
    if let IssueSelector::Uuid(expected_id) = selector
        && row.0 != *expected_id
    {
        // Preserve exact lowercase UUIDv7 identity under case-insensitive collations.
        return Err(not_found());
    }
    Ok(row)
}

fn load_issue_text(snapshot: &mut Snapshot, id: &str) -> Result<IssueTextRow, HiveError> {
    let mut rows: Vec<IssueTextRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT title, body, acceptance_criteria, hold_reason, \
                 IF(closed_at IS NULL, NULL, DATE_FORMAT(closed_at, '%Y-%m-%dT%H:%i:%s.%fZ')), \
                 closed_by_ref, resolution_note \
                 FROM issues AS OF '{commit}' WHERE id = ? LIMIT 2"
            )
        },
        (id,),
    )?;
    match rows.len() {
        1 => Ok(rows.pop().expect("single row")),
        0 => Err(not_found()),
        _ => Err(HiveError::new(
            ErrorCode::SchemaIncompatible,
            "Dolt returned multiple rows for a unique Issue selector.",
        )),
    }
}

fn not_found() -> HiveError {
    HiveError::new(
        ErrorCode::NotFound,
        "The requested Issue was not found at the selected Dolt commit.",
    )
}

fn validate_issue(issue: &IssueDetail, diagnostics: &mut Diagnostics) {
    let id = issue.id.as_str();
    diagnostics.check_id("issues", id, &[]);
    if issue.number == 0 {
        diagnostics.add(
            "INVALID_ISSUE_NUMBER",
            "issues",
            &[id],
            "Issue numbers must be positive and unique within the workspace.",
        );
    }
    if !is_uuid_v7(&issue.area_id) {
        diagnostics.check_id("issues", &issue.area_id, &[id]);
    }
    if let Some(parent_id) = issue.parent_id.as_deref() {
        if !is_uuid_v7(parent_id) {
            diagnostics.check_id("issues", parent_id, &[id]);
        } else if parent_id == id {
            diagnostics.add(
                "ISSUE_PARENT_CYCLE",
                "issues",
                &[id],
                "Issue parentage contains a cycle.",
            );
        }
    }
    if !is_issue_type(&issue.kind) {
        diagnostics.add(
            "INVALID_ISSUE_TYPE",
            "issues",
            &[id],
            "Use a canonical lowercase Issue type.",
        );
    }
    let Some(status) = IssueStatus::parse(&issue.status) else {
        diagnostics.add(
            "INVALID_ISSUE_STATUS",
            "issues",
            &[id],
            "Use a canonical lowercase Issue status.",
        );
        return;
    };
    if let Some(priority) = issue.priority
        && !(0..=3).contains(&priority)
    {
        diagnostics.add(
            "INVALID_ISSUE_PRIORITY",
            "issues",
            &[id],
            "Issue priority must be between 0 and 3 when set.",
        );
    }
    let closure_present = [
        issue.closed_at.is_some(),
        issue.closed_by_ref.is_some(),
        issue.resolution_note.is_some(),
    ];
    if closure_present != [status.is_terminal(); 3] {
        diagnostics.add(
            "INVALID_ISSUE_CLOSURE",
            "issues",
            &[id],
            "Done and cancelled Issues require all closure fields. Other statuses require them to be unset.",
        );
    }
}

fn load_claim_id(
    snapshot: &mut Snapshot,
    issue: &IssueDetail,
    diagnostics: &mut Diagnostics,
) -> Result<Option<String>, HiveError> {
    let rows: Vec<(String, String)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT issue_id, claim_id FROM issue_claims AS OF '{commit}' \
                 WHERE issue_id = ? ORDER BY claim_id ASC LIMIT 2"
            )
        },
        (&issue.id,),
    )?;
    for (owner_id, claim_id) in &rows {
        if owner_id != &issue.id {
            diagnostics.check_id("issue_claims", owner_id, &[&issue.id]);
        }
        diagnostics.check_id("issue_claims", claim_id, &[&issue.id]);
    }
    let claimed = !rows.is_empty();
    let in_progress = IssueStatus::parse(&issue.status) == Some(IssueStatus::InProgress);
    if rows.len() > 1 || claimed != in_progress || (issue.kind == "epic" && in_progress) {
        diagnostics.add(
            "INVALID_CLAIM_STATE",
            "issue_claims",
            &[&issue.id],
            "An active Claim is required exactly when a non-epic Issue is in progress.",
        );
    }
    Ok(rows.into_iter().next().map(|(_, claim_id)| claim_id))
}

fn load_child_ids(
    snapshot: &mut Snapshot,
    issue_id: &str,
    diagnostics: &mut Diagnostics,
) -> Result<BoundedIds, HiveError> {
    let rows: Vec<(String, Option<String>)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT id, parent_id FROM issues AS OF '{commit}' \
                 WHERE parent_id = ? ORDER BY id ASC LIMIT {}",
                RELATION_REF_LIMIT + 1
            )
        },
        (issue_id,),
    )?;
    Ok(collect_bounded_ids(
        "issues",
        issue_id,
        rows.into_iter()
            .map(|(child_id, parent_id)| (child_id, parent_id.unwrap_or_default())),
        diagnostics,
    ))
}

fn load_relation_ids(
    snapshot: &mut Snapshot,
    table: &'static str,
    target_col: &'static str,
    owner_col: &'static str,
    order_by: &'static str,
    issue_id: &str,
    diagnostics: &mut Diagnostics,
) -> Result<BoundedIds, HiveError> {
    let rows: Vec<(String, String)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT {target_col}, {owner_col} FROM {table} AS OF '{commit}' \
                 WHERE {owner_col} = ? ORDER BY {order_by} LIMIT {}",
                RELATION_REF_LIMIT + 1
            )
        },
        (issue_id,),
    )?;
    Ok(collect_bounded_ids(table, issue_id, rows, diagnostics))
}

fn collect_bounded_ids(
    table: &'static str,
    issue_id: &str,
    rows: impl IntoIterator<Item = (String, String)>,
    diagnostics: &mut Diagnostics,
) -> BoundedIds {
    let mut items = Vec::new();
    let mut truncated = false;
    for (target_id, owner_id) in rows {
        if owner_id != issue_id {
            diagnostics.check_id(table, &owner_id, &[issue_id]);
        }
        if !is_uuid_v7(&target_id) {
            diagnostics.check_id(table, &target_id, &[issue_id]);
        }
        if items.len() == RELATION_REF_LIMIT {
            truncated = true;
            break;
        }
        items.push(target_id);
    }
    BoundedIds { items, truncated }
}

#[cfg(test)]
mod tests {
    use super::{IssueSelector, parse_selector};

    #[test]
    fn parses_canonical_uuidv7_and_positive_decimal_issue_selectors() {
        let uuid = "01900000-0000-7000-8000-00000000000a";
        assert_eq!(
            parse_selector(uuid).unwrap(),
            IssueSelector::Uuid(uuid.to_owned())
        );
        assert_eq!(parse_selector("#1").unwrap(), IssueSelector::Number(1));
        assert_eq!(parse_selector("#42").unwrap(), IssueSelector::Number(42));
        assert_eq!(
            parse_selector(&format!("#{}", u64::MAX)).unwrap(),
            IssueSelector::Number(u64::MAX)
        );
    }

    #[test]
    fn rejects_invalid_issue_selectors_without_echoing_input() {
        let uppercase = "01900000-0000-7000-8000-00000000000A";
        let uuidv4 = "01900000-0000-4000-8000-00000000000a";
        for bad in [
            "",
            "1",
            "42",
            "#",
            "#0",
            "#00",
            "#-1",
            "#+1",
            "#1a",
            "# 1",
            "#18446744073709551616",
            uppercase,
            uuidv4,
            "task-fixture",
            "project/task",
            "' OR '1'='1",
        ] {
            let err = parse_selector(bad).unwrap_err();
            assert!(
                matches!(err.code, crate::error::ErrorCode::InvalidArgument),
                "{bad:?}: {:?}",
                err.code
            );
            if !bad.is_empty() && bad != "#" {
                assert!(!err.to_string().contains(bad), "{bad:?}");
            }
        }
    }
}
