use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use super::json_object::{self, Object};
use super::scope_plan::{closed_raw_html_block, is_portable_scope};
use super::task_intake::{
    CandidateStatus, TitleProblem, TitleSource, parse_record, resolve_title, valid_title,
};
use super::{Reason, is_full_lowercase_oid};
use crate::cli::is_slug;
use crate::config_file::present;
use crate::dolt::is_uuid_v7;
use crate::dolt::issue_update::valid_text;
use crate::error::{ErrorCode, HiveError};

pub(super) type Key<'a> = (&'a str, &'a str);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskReview {
    #[serde(deserialize_with = "json_object::deserialize")]
    legacy_key: LegacyKey,
    source_git_blob_sha: String,
    area_id: String,
    #[serde(default, deserialize_with = "present")]
    issue_type: Option<IssueType>,
    #[serde(default, deserialize_with = "present")]
    title: Option<Object<TitleReview>>,
    pub(super) relations: Option<Relations>,
    #[serde(default, deserialize_with = "optional_object")]
    created_at: Option<BodyRange>,
    #[serde(default, deserialize_with = "optional_object")]
    updated_at: Option<BodyRange>,
    #[serde(default, deserialize_with = "supplied_object")]
    acceptance_criteria: Option<Option<BodyRange>>,
    #[serde(default, deserialize_with = "present")]
    todo_fallback: Option<Object<TodoFallback>>,
    #[serde(default, deserialize_with = "optional_object")]
    closure: Option<ClosureReview>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TodoFallback {
    BacklogWithoutCriteria {},
}

#[derive(Clone, Copy, Default, Serialize)]
#[serde(rename_all = "lowercase")]
enum IssueType {
    Epic,
    Feature,
    #[default]
    Task,
    Bug,
}

impl<'de> Deserialize<'de> for IssueType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kind = String::deserialize(deserializer)?;
        match kind.as_str() {
            "epic" => Ok(Self::Epic),
            "feature" => Ok(Self::Feature),
            "task" => Ok(Self::Task),
            "bug" => Ok(Self::Bug),
            _ => Err(de::Error::unknown_variant(
                &kind,
                &["epic", "feature", "task", "bug"],
            )),
        }
    }
}

impl IssueType {
    fn is_default(&self) -> bool {
        matches!(self, Self::Task)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Epic => "epic",
            Self::Feature => "feature",
            Self::Task => "task",
            Self::Bug => "bug",
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
enum TitleReview {
    Fallback { value: String },
    Override { value: String },
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
    Simple(#[serde(deserialize_with = "json_object::string_enum")] RelationDeclaration),
    Reviewed(Object<Obligations>),
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
    #[serde(default, deserialize_with = "supplied_object")]
    parent: Option<Option<LegacyKey>>,
    #[serde(default, deserialize_with = "optional_objects")]
    prerequisites: Option<Vec<LegacyKey>>,
    #[serde(default, deserialize_with = "optional_objects")]
    children: Option<Vec<ChildReview>>,
}

fn supplied_object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    optional_object(deserializer).map(Some)
}

fn optional_object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<Object<T>>::deserialize(deserializer).map(|object| object.map(|object| object.0))
}

fn optional_objects<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error> {
    Option::<Vec<Object<T>>>::deserialize(deserializer)
        .map(|objects| objects.map(|objects| objects.into_iter().map(|object| object.0).collect()))
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
    #[serde(deserialize_with = "json_object::deserialize")]
    pub(super) legacy_key: LegacyKey,
    #[serde(default, deserialize_with = "optional_object")]
    pub(super) scope_acceptance: Option<BodyRange>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosureReview {
    #[serde(default, deserialize_with = "optional_object")]
    closed_at: Option<BodyRange>,
    #[serde(default, deserialize_with = "optional_object")]
    closed_by_ref: Option<BodyRange>,
    #[serde(default, deserialize_with = "optional_object")]
    resolution_note: Option<BodyRange>,
}

/// Half-open UTF-8 byte range relative to the body after frontmatter.
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BodyRange {
    start: u32,
    end: u32,
}

impl BodyRange {
    pub(super) fn select(self, body: &str) -> Option<&str> {
        body.get(self.start as usize..self.end as usize)
    }
}

type ReviewsByBlob<'a> = HashMap<&'a str, Vec<&'a TaskReview>>;

pub(super) struct Reviews<'a>(HashMap<Key<'a>, ReviewsByBlob<'a>>);

impl<'a> Reviews<'a> {
    pub(super) fn new(
        reviews: &'a [TaskReview],
        keys: &HashMap<Key<'_>, usize>,
    ) -> Result<Self, HiveError> {
        let mut index: HashMap<Key<'a>, ReviewsByBlob<'a>> = HashMap::new();
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
                .or_default()
                .entry(&review.source_git_blob_sha)
                .or_default()
                .push(review);
        }
        Ok(Self(index))
    }

    pub(super) fn source_bound<'lookup>(
        &'lookup self,
        key: Key<'lookup>,
        oid: &str,
    ) -> &'lookup [&'a TaskReview] {
        self.0
            .get(&key)
            .and_then(|by_blob| by_blob.get(oid))
            .map_or(&[], Vec::as_slice)
    }

    pub(super) fn for_preparation(
        &self,
        key: Key<'_>,
        oid: &str,
        area_id: &str,
    ) -> Result<Option<&'a TaskReview>, Reason> {
        let Some(by_blob) = self.0.get(&key) else {
            return Ok(None);
        };
        if by_blob.len() > 1 || by_blob.values().any(|reviews| reviews.len() > 1) {
            return Err(Reason::ConflictingTaskReview);
        }
        by_blob
            .get(oid)
            .map(|reviews| reviews[0])
            .filter(|review| review.area_id == area_id)
            .map(Some)
            .ok_or(Reason::TaskReviewBindingMismatch)
    }
}

// Cache one copy per reviewed blob. The verified inventory bounds the total size
// of distinct blobs. Only evidence locators and digests reach the report.
pub(super) struct SourceBody {
    text: String,
    body_start: usize,
    source_title: Result<(String, TitleSource), TitleProblem>,
    body_sha256: String,
    priority: Option<u8>,
    pub(super) labels: Vec<String>,
    scope_acceptance: OnceCell<HashSet<BodyRange>>,
    criteria_indicators: OnceCell<bool>,
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
        let source_title = resolve_title(metadata.title.as_deref(), &metadata.description, body)
            .map(|(title, source)| (title.to_owned(), source));
        let priority = match metadata.priority.as_ref().and_then(Value::as_str) {
            Some("high") => Some(0),
            Some("normal") => Some(1),
            Some("low") => Some(2),
            _ => None,
        };
        Ok(Self {
            body_start: text.len() - body.len(),
            body_sha256: format!("{:x}", Sha256::digest(body)),
            source_title,
            priority,
            labels: metadata
                .tags
                .into_iter()
                .flatten()
                .map(|tag| tag.trim().to_owned())
                .collect(),
            scope_acceptance: OnceCell::new(),
            criteria_indicators: OnceCell::new(),
            text,
        })
    }

    fn body(&self) -> &str {
        &self.text[self.body_start..]
    }

    fn scope_acceptance(&self, range: BodyRange, key: &LegacyKey) -> Option<BodyEvidence> {
        let body = self.body();
        let (scope, work_id) = key.key();
        let expected =
            format!("Cancellation of `{scope}/{work_id}` was accepted as a scope change.");
        BodyEvidence::select(range, body, |text| {
            text == expected
                && self
                    .scope_acceptance
                    .get_or_init(|| acceptance_paragraphs(body))
                    .contains(&range)
        })
    }
}

fn acceptance_paragraphs(body: &str) -> HashSet<BodyRange> {
    let mut ranges = HashSet::new();
    let mut depth = 0;
    for (event, range) in Parser::new(body).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                let text = &body[range.clone()];
                if tag == Tag::HtmlBlock && !closed_raw_html_block(text) {
                    return HashSet::new();
                }
                if depth == 0 && tag == Tag::Paragraph && text.starts_with("Cancellation of `") {
                    let text = text.trim_end_matches(['\r', '\n']);
                    ranges.insert(BodyRange {
                        start: range.start as u32,
                        end: (range.start + text.len()) as u32,
                    });
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    ranges
}

// The bound review attests that the Task has no applicable criteria, which
// can be written in free prose or kept in a referenced source. Refuse known
// criteria markers in the README even when the review omitted a body range.
fn has_criteria_indicators(body: &str) -> bool {
    let mut blocks = Vec::new();
    let mut rendered = String::new();
    for event in Parser::new_ext(body, Options::ENABLE_TASKLISTS) {
        match event {
            Event::TaskListMarker(_) => return true,
            Event::Text(text) | Event::Code(text) => {
                if criteria_marker(&text) {
                    return true;
                }
                if blocks.last() == Some(&true) {
                    rendered.push_str(&text);
                }
                continue;
            }
            Event::Html(text) | Event::InlineHtml(text) if criteria_marker(&text) => return true,
            Event::SoftBreak | Event::HardBreak if blocks.last() == Some(&true) => {
                rendered.push('\n');
                continue;
            }
            Event::Start(tag) => {
                let Some(render_text) = criteria_block(tag.to_end()) else {
                    continue;
                };
                blocks.push(render_text);
            }
            Event::End(tag) if criteria_block(tag).is_some() => {
                blocks.pop();
            }
            Event::Rule => {}
            _ => continue,
        }

        // Containers separate prose runs. Tight list items can supply their
        // own inline text without a paragraph, including in nested lists.
        if !rendered.is_empty() && criteria_marker(&rendered) {
            return true;
        }
        rendered.clear();
    }
    false
}

fn criteria_block(tag: TagEnd) -> Option<bool> {
    match tag {
        TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::Item => Some(true),
        TagEnd::BlockQuote(_) | TagEnd::CodeBlock | TagEnd::HtmlBlock | TagEnd::List(_) => {
            Some(false)
        }
        _ => None,
    }
}

fn criteria_marker(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.contains("acceptance criteria")
        || normalized.contains("completion criteria")
        || normalized.contains("definition of done")
        || text.lines().any(|line| {
            let line = line.trim();
            line == "acceptance"
                || line.starts_with("acceptance:")
                || line == "criteria"
                || line.starts_with("criteria:")
        })
}

#[derive(Serialize)]
pub(super) struct Prepared {
    relations: PreparedRelations,
    body_sha256: String,
    #[serde(skip)]
    title: String,
    title_source: TitleSource,
    title_sha256: String,
    #[serde(skip_serializing_if = "IssueType::is_default")]
    issue_type: IssueType,
    priority: Option<u8>,
    created_at: Timestamp,
    updated_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_criteria: Option<BodyEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    todo_fallback: Option<TodoFallback>,
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

#[derive(Clone, Serialize)]
pub(super) struct Timestamp {
    value: String,
    body_range: BodyRange,
    #[serde(skip)]
    instant: OffsetDateTime,
}

impl Timestamp {
    pub(super) fn select(range: Option<BodyRange>, body: &str) -> Option<(Self, OffsetDateTime)> {
        let body_range = range?;
        let value = body_range.select(body)?;
        let instant = parse_historical_timestamp(value)?;
        Some((
            Self {
                value: value.to_owned(),
                body_range,
                instant,
            },
            instant,
        ))
    }
}

fn parse_historical_timestamp(value: &str) -> Option<OffsetDateTime> {
    // The time parser accepts any separator byte. Require T or t, a known
    // offset, and a timestamp representable by DATETIME(6).
    if !matches!(value.as_bytes().get(10), Some(b'T' | b't'))
        || value.len() > 32
        || value.ends_with("-00:00")
        || value.get(17..19) == Some("60")
    {
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
    (1000..=9999).contains(&instant.year()).then_some(instant)
}

#[derive(Serialize)]
struct BodyEvidence {
    body_range: BodyRange,
    sha256: String,
}

pub(crate) struct StagingFields<'a> {
    pub(crate) title: &'a str,
    pub(crate) title_sha256: &'a str,
    pub(crate) issue_type: &'static str,
    pub(crate) body: &'a str,
    pub(crate) body_sha256: &'a str,
    pub(crate) criteria: Option<&'a str>,
    pub(crate) todo_fallback: Option<TodoFallback>,
    pub(crate) priority: Option<u8>,
    pub(crate) labels: &'a [String],
    pub(crate) created_at: OffsetDateTime,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) closure: Option<StagingClosure<'a>>,
    pub(crate) parent: Option<Key<'a>>,
    pub(crate) prerequisites: Vec<Key<'a>>,
    pub(crate) scope_acceptance: Vec<ScopeAcceptance<'a>>,
}

pub(crate) struct StagingClosure<'a> {
    pub(crate) closed_at: OffsetDateTime,
    pub(crate) closed_by_ref: &'a str,
    pub(crate) resolution_note: &'a str,
    closed_at_evidence: &'a Timestamp,
}

impl StagingClosure<'_> {
    pub(super) fn reviewed_closed_at(&self, range: BodyRange) -> Option<&Timestamp> {
        (self.closed_at_evidence.body_range == range).then_some(self.closed_at_evidence)
    }
}

pub(crate) struct ScopeAcceptance<'a> {
    pub(crate) child: Key<'a>,
    pub(crate) sha256: &'a str,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl Prepared {
    pub(super) fn target_status(&self, source_status: CandidateStatus) -> CandidateStatus {
        if self.todo_fallback.is_some() {
            CandidateStatus::Backlog
        } else {
            source_status
        }
    }

    pub(super) fn staging_fields<'a>(&'a self, source: &'a SourceBody) -> StagingFields<'a> {
        let body = &source.text[source.body_start..];
        let selected = |range: BodyRange| range.select(body).expect("verified Task body range");
        let mut fields = StagingFields {
            title: &self.title,
            title_sha256: &self.title_sha256,
            issue_type: self.issue_type.as_str(),
            body,
            body_sha256: &self.body_sha256,
            criteria: self
                .acceptance_criteria
                .as_ref()
                .map(|evidence| selected(evidence.body_range)),
            todo_fallback: self.todo_fallback,
            priority: self.priority,
            labels: &source.labels,
            created_at: self.created_at.instant,
            updated_at: self.updated_at.instant,
            closure: self.closure.as_ref().map(|closure| StagingClosure {
                closed_at: closure.closed_at.instant,
                closed_by_ref: selected(closure.closed_by_ref.body_range),
                resolution_note: selected(closure.resolution_note.body_range),
                closed_at_evidence: &closure.closed_at,
            }),
            parent: None,
            prerequisites: Vec::new(),
            scope_acceptance: Vec::new(),
        };
        if let PreparedRelations::Reviewed {
            parent,
            prerequisites,
            children,
        } = &self.relations
        {
            fields.parent = parent.as_ref().map(LegacyKey::key);
            fields.prerequisites = prerequisites.iter().map(LegacyKey::key).collect();
            fields.scope_acceptance = children
                .iter()
                .filter_map(|child| {
                    let proof = child.scope_acceptance.as_ref()?;
                    Some(ScopeAcceptance {
                        child: child.legacy_key.key(),
                        sha256: &proof.sha256,
                        start: proof.body_range.start,
                        end: proof.body_range.end,
                    })
                })
                .collect();
        }
        fields
    }
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

pub(super) fn fits_text_column(text: &str) -> bool {
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
    fn prepare(&self, source: &SourceBody, terminal: bool) -> Option<PreparedRelations> {
        match self {
            Self::Simple(RelationDeclaration::NoEdges) if !terminal => {
                Some(PreparedRelations::Simple(RelationDeclaration::NoEdges))
            }
            Self::Reviewed(Object(obligations)) => {
                let parent = obligations.parent.as_ref()?.clone();
                let mut prerequisites = obligations.prerequisites.as_ref()?.clone();
                prerequisites.sort_unstable_by(|left, right| left.key().cmp(&right.key()));
                let reviewed_children = obligations.children.as_ref()?;
                let mut children = Vec::with_capacity(reviewed_children.len());
                for child in reviewed_children {
                    let scope_acceptance = match child.scope_acceptance {
                        Some(range) => Some(source.scope_acceptance(range, &child.legacy_key)?),
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
    pub(super) fn has_title(&self) -> bool {
        self.title.is_some()
    }

    fn selected_title<'a>(&'a self, source: &'a SourceBody) -> Option<(&'a str, TitleSource)> {
        match self.title.as_ref().map(|title| &title.0) {
            None => source
                .source_title
                .as_ref()
                .ok()
                .map(|(title, source)| (title.as_str(), *source)),
            Some(TitleReview::Override { value }) if valid_title(value) => {
                Some((value, TitleSource::ReviewedOverride))
            }
            Some(TitleReview::Fallback { value })
                if matches!(source.source_title, Err(TitleProblem::Unavailable))
                    && valid_title(value) =>
            {
                Some((value, TitleSource::ReviewedFallback))
            }
            _ => None,
        }
    }

    pub(super) fn prepare(
        &self,
        source: &SourceBody,
        status: CandidateStatus,
        check_labels_at_staging: bool,
    ) -> Result<Prepared, Vec<Reason>> {
        let body = source.body();
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
            .and_then(|review| review.prepare(source, terminal));
        let criteria_range = self.acceptance_criteria.flatten();
        let criteria =
            criteria_range.and_then(|range| BodyEvidence::select(range, body, valid_text));
        let todo_fallback = self
            .todo_fallback
            .as_ref()
            .map(|decision| decision.0)
            .filter(|_| {
                status == CandidateStatus::Todo
                    && self.acceptance_criteria.is_none()
                    && !*source
                        .criteria_indicators
                        .get_or_init(|| has_criteria_indicators(body))
            });
        let title = self.selected_title(source);
        let mut reasons = Vec::new();
        if title.is_none() {
            reasons.push(Reason::MissingOrConflictingTitle);
        }
        if dates.is_none()
            || (terminal && closure.is_none())
            || (!terminal && self.closure.is_some())
        {
            reasons.push(Reason::IncompleteHistoricalProvenance);
        }
        if relations.is_none() {
            reasons.push(status.relation_hold());
        }
        if self.todo_fallback.is_some() && todo_fallback.is_none() {
            reasons.push(Reason::InapplicableTodoFallback);
        }
        if criteria.is_none()
            && ((status == CandidateStatus::Todo && todo_fallback.is_none())
                || criteria_range.is_some())
        {
            reasons.push(Reason::UnverifiedAcceptanceCriteria);
        }
        if !check_labels_at_staging && !source.labels.is_empty() {
            reasons.push(Reason::UnverifiedTaskLabels);
        }
        if !reasons.is_empty() {
            return Err(reasons);
        }
        let ((created_at, _), (updated_at, _)) = dates.expect("validated dates");
        let (title, title_source) = title.expect("validated Task title");
        Ok(Prepared {
            relations: relations.expect("validated Task relations"),
            body_sha256: source.body_sha256.clone(),
            title: title.to_owned(),
            title_source,
            title_sha256: format!("{:x}", Sha256::digest(title)),
            issue_type: self.issue_type.unwrap_or_default(),
            priority: source.priority,
            created_at,
            updated_at,
            acceptance_criteria: criteria,
            todo_fallback,
            closure,
        })
    }
}
