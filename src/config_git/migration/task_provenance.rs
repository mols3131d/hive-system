use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use super::scope_plan::is_portable_scope;
use super::task_intake::{TitleSource, parse_record, resolve_title};
use super::{Reason, is_full_lowercase_oid};
use crate::cli::is_slug;
use crate::dolt::is_uuid_v7;
use crate::error::{ErrorCode, HiveError};

type Key<'a> = (&'a str, &'a str);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskReview {
    legacy_key: LegacyKey,
    source_git_blob_sha: String,
    area_id: String,
    relations: Option<Relations>,
    created_at: Option<BodyRange>,
    updated_at: Option<BodyRange>,
    acceptance_criteria: Option<BodyRange>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyKey {
    scope: String,
    work_id: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Relations {
    NoEdges,
    Unresolved,
}

/// Half-open UTF-8 byte range relative to the body after frontmatter.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BodyRange {
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
            let key = (
                review.legacy_key.scope.as_str(),
                review.legacy_key.work_id.as_str(),
            );
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
        scope: &str,
        work_id: &str,
        oid: &str,
        area_id: &str,
    ) -> Result<Option<&'a TaskReview>, Reason> {
        match self.0.get(&(scope, work_id)) {
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
pub(super) struct Prepared<'a> {
    relations: Relations,
    body_sha256: &'a str,
    title_source: TitleSource,
    title_sha256: &'a str,
    priority: Option<u8>,
    created_at: Timestamp<'a>,
    updated_at: Timestamp<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_criteria: Option<Criteria>,
}

#[derive(Serialize)]
struct Timestamp<'a> {
    value: &'a str,
    body_range: BodyRange,
}

impl<'a> Timestamp<'a> {
    fn select(range: Option<BodyRange>, body: &'a str) -> Option<(Self, OffsetDateTime)> {
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
        (1000..=9999)
            .contains(&instant.year())
            .then_some((Self { value, body_range }, instant))
    }
}

#[derive(Serialize)]
struct Criteria {
    body_range: BodyRange,
    sha256: String,
}

impl TaskReview {
    pub(super) fn prepare<'a>(
        &self,
        source: &'a SourceBody,
        needs_criteria: bool,
    ) -> Result<Prepared<'a>, Vec<Reason>> {
        let body = &source.text[source.body_start..];
        let dates = Timestamp::select(self.created_at, body)
            .zip(Timestamp::select(self.updated_at, body))
            .filter(|((_, created), (_, updated))| created <= updated);
        let criteria = self.acceptance_criteria.and_then(|body_range| {
            let text = body_range.select(body)?;
            // acceptance_criteria is a target TEXT column.
            (text.len() <= 65_535
                && !text.trim().is_empty()
                && !text
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n')))
            .then(|| Criteria {
                body_range,
                sha256: format!("{:x}", Sha256::digest(text)),
            })
        });
        let mut reasons = Vec::new();
        if dates.is_none() {
            reasons.push(Reason::IncompleteHistoricalProvenance);
        }
        if !matches!(self.relations, Some(Relations::NoEdges)) {
            reasons.push(Reason::UnresolvedTaskRelations);
        }
        if criteria.is_none() && (needs_criteria || self.acceptance_criteria.is_some()) {
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
            relations: Relations::NoEdges,
            body_sha256: &source.body_sha256,
            title_source: source.title_source,
            title_sha256: &source.title_sha256,
            priority: source.priority,
            created_at,
            updated_at,
            acceptance_criteria: criteria,
        })
    }
}
