use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::Reason;
use super::artifact_archive::ArchiveReview;
use super::directive_artifact_intake::ArtifactKind;
use super::json_object;
use super::prepared_records::{ArtifactFields, Fields};
use super::reviewed_records::{actor_ref, canonical_utc, sha256};
use super::scope_plan::portable_reference;
use super::task_intake::StagingTask;
use super::task_provenance::{BodyRange, Key, LegacyKey, Timestamp};

pub(super) struct TaskContext<'a> {
    workspace_id: &'a str,
    prepared: HashMap<Key<'a>, &'a StagingTask<'a>>,
}

impl<'a> TaskContext<'a> {
    pub(super) fn task(&self, key: Key<'_>) -> Option<&StagingTask<'_>> {
        self.prepared.get(&key).copied()
    }

    pub(super) fn new(workspace_id: &'a str, tasks: Option<&'a [StagingTask<'a>]>) -> Self {
        Self {
            workspace_id,
            prepared: tasks
                .unwrap_or_default()
                .iter()
                .map(|task| (task.key, task))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkArtifactReview {
    #[serde(deserialize_with = "json_object::string_enum")]
    pub(super) artifact_kind: ArtifactKind,
    source_ref: String,
    author_ref: String,
    #[serde(default, deserialize_with = "json_object::optional_object")]
    created_at: Option<BodyRange>,
    #[serde(default, deserialize_with = "json_object::optional_object")]
    substantive_updated_at: Option<BodyRange>,
    #[serde(deserialize_with = "json_object::deserialize")]
    parent_task: ParentReview,
    #[serde(default, deserialize_with = "json_object::optional_string_enum")]
    issue_link: Option<IssueLinkDecision>,
    area_link: Option<String>,
    archive: ArchiveReview,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentReview {
    #[serde(deserialize_with = "json_object::deserialize")]
    legacy_key: LegacyKey,
    workspace_id: String,
    area_id: String,
    source_path: String,
    source_git_blob_sha: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum IssueLinkDecision {
    Retain,
    Unresolved,
}

#[derive(Serialize)]
pub(crate) struct IssueLink {
    decision: &'static str,
    legacy_key: LegacyKey,
    pub(crate) workspace_id: String,
    pub(crate) area_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) joined_area_id: Option<String>,
    #[serde(skip)]
    pub(crate) source_path: String,
    source_path_sha256: String,
    pub(crate) source_git_blob_sha: String,
}

impl IssueLink {
    pub(crate) fn key(&self) -> (&str, &str) {
        self.legacy_key.key()
    }
}

pub(crate) struct WorkScope {
    pub(crate) issue_link: IssueLink,
    pub(super) created_at: Timestamp,
    pub(super) substantive_updated_at: Timestamp,
}

impl WorkArtifactReview {
    pub(super) fn prepare(
        &self,
        kind: ArtifactKind,
        body: &str,
        parent_bundle: &str,
        tasks: &TaskContext<'_>,
    ) -> Result<Fields, Reason> {
        let expected = &self.parent_task;
        let actual = tasks
            .prepared
            .get(&expected.legacy_key.key())
            .filter(|actual| {
                expected.workspace_id == tasks.workspace_id
                    && expected.source_path == actual.source_path
                    && actual.source_path.strip_suffix("/README.md") == Some(parent_bundle)
                    && expected.source_git_blob_sha == actual.source_git_blob_sha
                    && expected.area_id == actual.area_id
            })
            .ok_or(Reason::ParentTaskQuarantined)?;
        if !matches!(self.issue_link, Some(IssueLinkDecision::Retain))
            || self
                .area_link
                .as_deref()
                .is_some_and(|area| area != actual.area_id)
        {
            return Err(Reason::UnresolvedArtifactLinks);
        }
        if self.artifact_kind != kind {
            return Err(Reason::UnclassifiedArtifactKind);
        }
        if !actor_ref(&self.author_ref) {
            return Err(Reason::UnverifiedArtifactAuthor);
        }
        if !matches!(actual.status, "backlog" | "todo") {
            return Err(Reason::UnverifiedArtifactRetention);
        }
        if !portable_reference(&self.source_ref) {
            return Err(Reason::IncompleteHistoricalProvenance);
        }
        let ((created_at, created), (substantive_updated_at, updated)) =
            Timestamp::select(self.created_at, body)
                .zip(Timestamp::select(self.substantive_updated_at, body))
                .filter(|((_, created), (_, updated))| created <= updated)
                .ok_or(Reason::IncompleteHistoricalProvenance)?;
        Ok(Fields::Artifact(ArtifactFields {
            kind,
            author_ref: self.author_ref.clone(),
            source_ref: self.source_ref.clone(),
            created_at: canonical_utc(created),
            substantive_updated_at: canonical_utc(updated),
            standalone_links: None,
            archive: self.archive.prepare(Some(body), updated)?,
            work: Some(Box::new(WorkScope {
                issue_link: IssueLink {
                    decision: "retain",
                    legacy_key: expected.legacy_key.clone(),
                    workspace_id: tasks.workspace_id.to_owned(),
                    area_id: actual.area_id.to_owned(),
                    joined_area_id: self.area_link.clone(),
                    source_path: actual.source_path.to_owned(),
                    source_path_sha256: sha256(actual.source_path),
                    source_git_blob_sha: actual.source_git_blob_sha.to_owned(),
                },
                created_at,
                substantive_updated_at,
            })),
        }))
    }
}
