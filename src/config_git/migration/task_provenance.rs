use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use super::scope_plan::is_portable_scope;
use super::task_intake::{CandidateStatus, TitleSource, parse_record, resolve_title};
use super::{Reason, is_full_lowercase_oid};
use crate::cli::is_slug;
use crate::dolt::is_uuid_v7;
use crate::error::{ErrorCode, HiveError};

pub(super) type Key<'a> = (&'a str, &'a str);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskReview {
    legacy_key: LegacyKey,
    source_git_blob_sha: String,
    area_id: String,
    pub(super) relations: Option<Relations>,
    created_at: Option<BodyRange>,
    updated_at: Option<BodyRange>,
    acceptance_criteria: Option<BodyRange>,
    closure: Option<ClosureReview>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyKey {
    scope: String,
    work_id: String,
}

impl LegacyKey {
    pub(super) fn key(&self) -> Key<'_> {
        (&self.scope, &self.work_id)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Relations {
    Simple(RelationDeclaration),
    Reviewed(Obligations),
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RelationDeclaration {
    NoEdges,
    Unresolved,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Obligations {
    #[serde(default, deserialize_with = "declared_parent")]
    parent: Option<Option<LegacyKey>>,
    prerequisites: Option<Vec<LegacyKey>>,
    children: Option<Vec<ChildReview>>,
}

fn declared_parent<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<LegacyKey>>, D::Error> {
    Option::deserialize(deserializer).map(Some)
}

impl Obligations {
    pub(super) fn parent(&self) -> Option<&LegacyKey> {
        self.parent.as_ref().and_then(Option::as_ref)
    }

    pub(super) fn prerequisites(&self) -> &[LegacyKey] {
        self.prerequisites.as_deref().unwrap_or_default()
    }

    pub(super) fn children(&self) -> &[ChildReview] {
        self.children.as_deref().unwrap_or_default()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ChildReview {
    pub(super) legacy_key: LegacyKey,
    pub(super) scope_acceptance: Option<BodyRange>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosureReview {
    closed_at: Option<BodyRange>,
    closed_by_ref: Option<BodyRange>,
    resolution_note: Option<BodyRange>,
}

/// Half-open UTF-8 byte range relative to the body after frontmatter.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BodyRange {
    start: u32,
    end: u32,
}

impl BodyRange {
    fn select(self, body: &str) -> Option<&str> {
        body.get(self.start as usize..self.end as usize)
    }
}

pub(super) struct Reviews<'a>(HashMap<Key<'a>, Option<&'a TaskReview>>);

impl<'a> Reviews<'a> {
    pub(super) fn new(
        reviews: &'a [TaskReview],
        keys: &HashMap<Key<'_>, usize>,
    ) -> Result<Self, HiveError> {
        let mut index = HashMap::new();
        for review in reviews {
            let key = review.legacy_key.key();
            if !is_portable_scope(key.0) || !is_slug(key.1) || !keys.contains_key(&key) {
                return Err(HiveError::new(
                    ErrorCode::InvalidConfig,
                    "A Task review key does not name a supported Task in the verified source.",
                ));
            }
            if !is_full_lowercase_oid(&review.source_git_blob_sha) || !is_uuid_v7(&review.area_id) {
                return Err(HiveError::new(
                    ErrorCode::InvalidConfig,
                    "A Task review has an invalid Git blob ID or Area ID.",
                ));
            }
            index
                .entry(key)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(review));
        }
        Ok(Self(index))
    }

    pub(super) fn get(
        &self,
        key: Key<'_>,
        oid: &str,
        area_id: &str,
    ) -> Result<Option<&'a TaskReview>, Reason> {
        match self.0.get(&key) {
            None => Ok(None),
            Some(None) => Err(Reason::ConflictingTaskReview),
            Some(Some(review))
                if review.source_git_blob_sha == oid && review.area_id == area_id =>
            {
                Ok(Some(review))
            }
            _ => Err(Reason::TaskReviewBindingMismatch),
        }
    }
}

// Cache one copy per reviewed blob. The verified inventory bounds the total size
// of distinct blobs. Only evidence locators and digests reach the report.
pub(super) struct SourceBody {
    text: String,
    body_start: usize,
    body_sha256: String,
    title_source: TitleSource,
    title_sha256: String,
    priority: Option<u8>,
    has_tags: bool,
}

impl SourceBody {
    pub(super) fn from_verified_blob(bytes: Vec<u8>) -> Result<Self, HiveError> {
        let invalid = || {
            HiveError::new(
                ErrorCode::InvalidRecord,
                "Could not resolve the verified Task source.",
            )
        };
        let text = String::from_utf8(bytes).map_err(|_| invalid())?;
        let (metadata, body) = parse_record(&text).ok_or_else(invalid)?;
        let (title, title_source) =
            resolve_title(metadata.title.as_deref(), &metadata.description, body)
                .ok_or_else(invalid)?;
        let priority = match metadata.priority.as_ref().and_then(Value::as_str) {
            Some("high") => Some(0),
            Some("normal") => Some(1),
            Some("low") => Some(2),
            _ => None,
        };
        Ok(Self {
            body_start: text.len() - body.len(),
            body_sha256: format!("{:x}", Sha256::digest(body)),
            title_source,
            title_sha256: format!("{:x}", Sha256::digest(title)),
            priority,
            has_tags: metadata.tags.is_some(),
            text,
        })
    }
}

#[derive(Serialize)]
pub(super) struct Prepared {
    relations: PreparedRelations,
    body_sha256: String,
    title_source: TitleSource,
    title_sha256: String,
    priority: Option<u8>,
    created_at: Timestamp,
    updated_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_criteria: Option<BodyEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    closure: Option<PreparedClosure>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum PreparedRelations {
    Simple(RelationDeclaration),
    Reviewed {
        parent: Option<LegacyKey>,
        prerequisites: Vec<LegacyKey>,
        children: Vec<PreparedChild>,
    },
}

#[derive(Serialize)]
struct PreparedChild {
    legacy_key: LegacyKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_acceptance: Option<BodyEvidence>,
}

#[derive(Serialize)]
struct PreparedClosure {
    closed_at: Timestamp,
    closed_by_ref: BodyEvidence,
    resolution_note: BodyEvidence,
}

#[derive(Serialize)]
struct Timestamp {
    value: String,
    body_range: BodyRange,
}

impl Timestamp {
    fn select(range: Option<BodyRange>, body: &str) -> Option<(Self, OffsetDateTime)> {
        let body_range = range?;
        let value = body_range.select(body)?;
        // Require an explicit offset and precision representable by DATETIME(6).
        // RFC 3339 permits an unknown offset and leap seconds, which aren't
        // usable as exact timestamps in the target.
        if value.len() > 32 || value.ends_with("-00:00") || value.get(17..19) == Some("60") {
            return None;
        }
        if let Some((_, fraction)) = value.split_once('.')
            && fraction.bytes().take_while(u8::is_ascii_digit).count() > 6
        {
            return None;
        }
        let instant = OffsetDateTime::parse(value, &Rfc3339)
            .ok()?
            .checked_to_offset(UtcOffset::UTC)?;
        (1000..=9999).contains(&instant.year()).then(|| {
            (
                Self {
                    value: value.to_owned(),
                    body_range,
                },
                instant,
            )
        })
    }
}

#[derive(Serialize)]
struct BodyEvidence {
    body_range: BodyRange,
    sha256: String,
}

impl BodyEvidence {
    fn select(body_range: BodyRange, body: &str, valid: impl FnOnce(&str) -> bool) -> Option<Self> {
        let text = body_range.select(body)?;
        (!text.trim().is_empty() && valid(text)).then(|| Self {
            body_range,
            sha256: format!("{:x}", Sha256::digest(text)),
        })
    }
}

fn fits_text_column(text: &str) -> bool {
    text.len() <= 65_535
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
}

impl ClosureReview {
    fn prepare(&self, body: &str, created: OffsetDateTime) -> Option<PreparedClosure> {
        let (closed_at, closed) = Timestamp::select(self.closed_at, body)?;
        if closed < created {
            return None;
        }
        Some(PreparedClosure {
            closed_at,
            closed_by_ref: BodyEvidence::select(self.closed_by_ref?, body, |actor| {
                actor == actor.trim()
                    && actor.chars().count() <= 255
                    && !actor.chars().any(char::is_control)
            })?,
            resolution_note: BodyEvidence::select(self.resolution_note?, body, fits_text_column)?,
        })
    }
}

impl Relations {
    fn prepare(&self, body: &str, terminal: bool) -> Option<PreparedRelations> {
        match self {
            Self::Simple(RelationDeclaration::NoEdges) if !terminal => {
                Some(PreparedRelations::Simple(RelationDeclaration::NoEdges))
            }
            Self::Reviewed(obligations) => {
                let parent = obligations.parent.as_ref()?.clone();
                let mut prerequisites = obligations.prerequisites.as_ref()?.clone();
                prerequisites.sort_unstable_by(|left, right| left.key().cmp(&right.key()));
                let reviewed_children = obligations.children.as_ref()?;
                let mut children = Vec::with_capacity(reviewed_children.len());
                for child in reviewed_children {
                    let scope_acceptance = match child.scope_acceptance {
                        Some(range) => Some(BodyEvidence::select(range, body, fits_text_column)?),
                        None => None,
                    };
                    children.push(PreparedChild {
                        legacy_key: child.legacy_key.clone(),
                        scope_acceptance,
                    });
                }
                children.sort_unstable_by(|left, right| {
                    left.legacy_key.key().cmp(&right.legacy_key.key())
                });
                Some(PreparedRelations::Reviewed {
                    parent,
                    prerequisites,
                    children,
                })
            }
            _ => None,
        }
    }
}

impl TaskReview {
    pub(super) fn prepare(
        &self,
        source: &SourceBody,
        status: CandidateStatus,
    ) -> Result<Prepared, Vec<Reason>> {
        let body = &source.text[source.body_start..];
        let terminal = status.is_terminal();
        let dates = Timestamp::select(self.created_at, body)
            .zip(Timestamp::select(self.updated_at, body))
            .filter(|((_, created), (_, updated))| created <= updated);
        let closure = self.closure.as_ref().and_then(|review| {
            let ((_, created), _) = dates.as_ref()?;
            review.prepare(body, *created)
        });
        let relations = self
            .relations
            .as_ref()
            .and_then(|review| review.prepare(body, terminal));
        let criteria = self
            .acceptance_criteria
            .and_then(|range| BodyEvidence::select(range, body, fits_text_column));
        let mut reasons = Vec::new();
        if dates.is_none()
            || (terminal && closure.is_none())
            || (!terminal && self.closure.is_some())
        {
            reasons.push(Reason::IncompleteHistoricalProvenance);
        }
        if relations.is_none() {
            reasons.push(status.relation_hold());
        }
        if criteria.is_none()
            && (status == CandidateStatus::Todo || self.acceptance_criteria.is_some())
        {
            reasons.push(Reason::UnverifiedAcceptanceCriteria);
        }
        if source.has_tags {
            reasons.push(Reason::UnverifiedTaskLabels);
        }
        if !reasons.is_empty() {
            return Err(reasons);
        }
        let ((created_at, _), (updated_at, _)) = dates.expect("validated dates");
        Ok(Prepared {
            relations: relations.expect("validated Task relations"),
            body_sha256: source.body_sha256.clone(),
            title_source: source.title_source,
            title_sha256: source.title_sha256.clone(),
            priority: source.priority,
            created_at,
            updated_at,
            acceptance_criteria: criteria,
            closure,
        })
    }
}
