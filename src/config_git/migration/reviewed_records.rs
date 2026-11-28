use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::directive_artifact_intake::{ArtifactKind, Candidate, DirectiveStatus};
use super::directive_history::{DirectiveHistory, Edges};
use super::prepared_records::{ArtifactFields, DirectiveFields, Fields};
use super::scope_plan::portable_reference;
use super::standalone_artifact::{JoinContext, LinkReview};
use super::work_artifact::{TaskContext, WorkArtifactReview};
use super::{Inventory, Reason};
use crate::cli::is_slug;
use crate::dolt::is_uuid_v7;
use crate::error::{ErrorCode, HiveError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Review {
    source_path: String,
    source_git_blob_sha: String,
    pub(super) target_id: String,
    #[serde(deserialize_with = "super::json_object::deserialize")]
    provenance: Provenance,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Provenance {
    ProposedDirective {
        area_id: String,
        source_ref: String,
        issuer_ref: String,
        created_at: String,
        updated_at: String,
        supersession: AbsenceReview,
        approval_and_validity: AbsenceReview,
    },
    DirectiveHistory(DirectiveHistory),
    WorkScopedArtifact(WorkArtifactReview),
    StandaloneArtifact {
        artifact_kind: ArtifactKind,
        source_ref: String,
        author_ref: String,
        created_at: String,
        substantive_updated_at: String,
        links: LinkReview,
        archive: AbsenceReview,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AbsenceReview {
    None,
    Unresolved,
}

pub(super) fn deserialize_present<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Review>>, D::Error> {
    super::json_object::deserialize_vec(deserializer).map(Some)
}

pub(super) fn bind<'a>(
    reviews: &'a [Review],
    inventory: &Inventory,
) -> Result<BTreeMap<&'a str, &'a Review>, HiveError> {
    let mut by_path = BTreeMap::new();
    let mut target_ids = HashSet::new();
    for review in reviews {
        if !inventory
            .entries
            .get(&review.source_path)
            .is_some_and(|entry| !entry.is_directory() && entry.oid == review.source_git_blob_sha)
        {
            return Err(invalid(
                "Each reviewed record must match a committed path and blob.",
            ));
        }
        if !is_uuid_v7(&review.target_id)
            || !target_ids.insert(&review.target_id)
            || by_path
                .insert(review.source_path.as_str(), review)
                .is_some()
        {
            return Err(invalid(
                "Reviewed records require distinct paths and distinct lowercase UUIDv7 target IDs.",
            ));
        }
    }
    Ok(by_path)
}

impl Review {
    pub(super) fn is_work_scoped(&self) -> bool {
        matches!(self.provenance, Provenance::WorkScopedArtifact(_))
    }

    pub(super) fn needs_tasks(&self) -> bool {
        match &self.provenance {
            Provenance::WorkScopedArtifact(_) => true,
            Provenance::StandaloneArtifact { links, .. } => links.needs_tasks(),
            _ => false,
        }
    }

    pub(super) fn prepare_work_scoped(
        &self,
        candidate: &Candidate,
        body: &str,
        parent_bundle: &str,
        tasks: &TaskContext<'_>,
    ) -> Result<Fields, Reason> {
        let (Provenance::WorkScopedArtifact(review), Candidate::Artifact { kind, .. }) =
            (&self.provenance, candidate)
        else {
            return Err(Reason::UnsupportedReviewedRecord);
        };
        review.prepare(*kind, body, parent_bundle, tasks)
    }

    pub(super) fn directive_edges(&self) -> Option<&Edges> {
        match &self.provenance {
            Provenance::DirectiveHistory(history) => Some(&history.supersession),
            _ => None,
        }
    }

    pub(super) fn prepare(
        &self,
        candidate: &Candidate,
        observed_at: Option<OffsetDateTime>,
        ctx: &JoinContext<'_>,
    ) -> Result<Fields, Reason> {
        match (&self.provenance, candidate) {
            (
                Provenance::ProposedDirective {
                    area_id,
                    source_ref,
                    issuer_ref,
                    created_at,
                    updated_at,
                    supersession,
                    approval_and_validity,
                },
                Candidate::Directive {
                    status: DirectiveStatus::Proposed,
                    area_id: mapped,
                    ..
                },
            ) => {
                if area_id != mapped {
                    return Err(Reason::AmbiguousScopeOrCollision);
                }
                if matches!(supersession, AbsenceReview::Unresolved) {
                    return Err(Reason::UnresolvedSupersessionGraph);
                }
                if matches!(approval_and_validity, AbsenceReview::Unresolved) {
                    return Err(Reason::UnverifiedDirectiveValidity);
                }
                validate_historical_source(source_ref, created_at, updated_at)?;
                if !actor_ref(issuer_ref) {
                    return Err(Reason::IncompleteHistoricalProvenance);
                }
                Ok(Fields::Directive(DirectiveFields {
                    status: DirectiveStatus::Proposed,
                    area_id: area_id.clone(),
                    source_ref: source_ref.clone(),
                    issuer_ref: issuer_ref.clone(),
                    created_at: created_at.clone(),
                    updated_at: updated_at.clone(),
                    supersedes_id: None,
                    approval_ref: None,
                    valid_from: None,
                    expires_at: None,
                }))
            }
            (
                Provenance::DirectiveHistory(history),
                Candidate::Directive {
                    status, area_id, ..
                },
            ) => history.prepare(*status, area_id, observed_at),
            (
                Provenance::StandaloneArtifact {
                    artifact_kind,
                    source_ref,
                    author_ref,
                    created_at,
                    substantive_updated_at,
                    links,
                    archive,
                },
                Candidate::Artifact { kind, .. },
            ) => {
                if artifact_kind != kind {
                    return Err(Reason::UnclassifiedArtifactKind);
                }
                let standalone_links = links.prepare(ctx)?;
                if !actor_ref(author_ref) {
                    return Err(Reason::UnverifiedArtifactAuthor);
                }
                if matches!(archive, AbsenceReview::Unresolved) {
                    return Err(Reason::UnverifiedArtifactRetention);
                }
                validate_historical_source(source_ref, created_at, substantive_updated_at)?;
                Ok(Fields::Artifact(ArtifactFields {
                    kind: *kind,
                    source_ref: source_ref.clone(),
                    author_ref: author_ref.clone(),
                    created_at: created_at.clone(),
                    substantive_updated_at: substantive_updated_at.clone(),
                    work: None,
                    standalone_links,
                }))
            }
            _ => Err(Reason::UnsupportedReviewedRecord),
        }
    }
}

pub(super) fn validate_historical_source(
    source_ref: &str,
    created: &str,
    updated: &str,
) -> Result<(), Reason> {
    if !portable_reference(source_ref)
        || !matches!(timestamp(created).zip(timestamp(updated)), Some((created, updated)) if created <= updated)
    {
        return Err(Reason::IncompleteHistoricalProvenance);
    }
    Ok(())
}

pub(super) fn timestamp(value: &str) -> Option<OffsetDateTime> {
    let bytes = value.as_bytes();
    // DATETIME(6) stores UTC wall time to microseconds. Refuse any loss of precision.
    if !value.ends_with('Z')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(17..19) == Some(b"60")
        || !(value.len() == 20
            || ((22..=27).contains(&value.len()) && bytes.get(19) == Some(&b'.')))
    {
        return None;
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .filter(|time| time.year() >= 1000)
}

pub(super) fn actor_ref(value: &str) -> bool {
    value.len() <= 255
        && value.split_once(':').map_or_else(
            || is_slug(value),
            |(namespace, actor)| is_slug(namespace) && is_slug(actor),
        )
}

pub(super) fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value))
}

pub(super) fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidConfig, message)
}
