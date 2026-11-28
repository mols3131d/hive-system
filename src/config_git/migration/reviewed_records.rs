use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Deserializer, de, de::value::StrDeserializer};
use serde_json::{Value, json, value::RawValue};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::directive_artifact_intake::{ArtifactKind, Candidate, DirectiveStatus};
use super::scope_plan::portable_reference;
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
    #[serde(deserialize_with = "deserialize_provenance")]
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
        #[serde(deserialize_with = "string_enum")]
        supersession: AbsenceReview,
        #[serde(deserialize_with = "string_enum")]
        approval_and_validity: AbsenceReview,
    },
    StandaloneArtifact {
        #[serde(deserialize_with = "string_enum")]
        artifact_kind: ArtifactKind,
        source_ref: String,
        author_ref: String,
        created_at: String,
        substantive_updated_at: String,
        #[serde(deserialize_with = "string_enum")]
        links: AbsenceReview,
        #[serde(deserialize_with = "string_enum")]
        archive: AbsenceReview,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AbsenceReview {
    None,
    Unresolved,
}

fn string_enum<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    let label = String::deserialize(deserializer)?;
    T::deserialize(StrDeserializer::new(&label))
}

pub(super) fn deserialize_present<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Review>>, D::Error> {
    Vec::<&RawValue>::deserialize(deserializer)?
        .into_iter()
        .map(parse_json_object)
        .collect::<Result<_, _>>()
        .map(Some)
}

fn deserialize_provenance<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Provenance, D::Error> {
    parse_json_object(<&RawValue>::deserialize(deserializer)?)
}

fn parse_json_object<'de, T: Deserialize<'de>, E: de::Error>(value: &'de RawValue) -> Result<T, E> {
    if !value.get().trim_start().starts_with('{') {
        return Err(E::custom("Expected a JSON object."));
    }
    // Decode the original text to preserve duplicate-key checks.
    serde_json::from_str(value.get()).map_err(E::custom)
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
    pub(super) fn prepare(&self, candidate: &Candidate) -> Result<Value, Reason> {
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
                let source_hash = historical_source_digest(source_ref, created_at, updated_at)?;
                if !actor_ref(issuer_ref) {
                    return Err(Reason::IncompleteHistoricalProvenance);
                }
                Ok(json!({
                    "source_ref_sha256": source_hash,
                    "issuer_ref_sha256": sha256(issuer_ref),
                    "created_at": created_at,
                    "updated_at": updated_at,
                    "supersedes_id": null,
                    "approval_ref": null,
                    "valid_from": null,
                    "expires_at": null,
                }))
            }
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
                if matches!(links, AbsenceReview::Unresolved) {
                    return Err(Reason::UnresolvedArtifactLinks);
                }
                if !actor_ref(author_ref) {
                    return Err(Reason::UnverifiedArtifactAuthor);
                }
                if matches!(archive, AbsenceReview::Unresolved) {
                    return Err(Reason::UnverifiedArtifactRetention);
                }
                let source_hash =
                    historical_source_digest(source_ref, created_at, substantive_updated_at)?;
                Ok(json!({
                    "source_ref_sha256": source_hash,
                    "author_ref_sha256": sha256(author_ref),
                    "created_at": created_at,
                    "substantive_updated_at": substantive_updated_at,
                    "scope": "standalone",
                    "archived_at": null,
                    "archive_reason": null,
                }))
            }
            _ => Err(Reason::UnsupportedReviewedRecord),
        }
    }
}

fn historical_source_digest(
    source_ref: &str,
    created: &str,
    updated: &str,
) -> Result<String, Reason> {
    if !portable_reference(source_ref)
        || !matches!(timestamp(created).zip(timestamp(updated)), Some((created, updated)) if created <= updated)
    {
        return Err(Reason::IncompleteHistoricalProvenance);
    }
    Ok(sha256(source_ref))
}

fn timestamp(value: &str) -> Option<OffsetDateTime> {
    let bytes = value.as_bytes();
    // DATETIME(6) stores UTC wall time to microseconds. Refuse any loss of precision.
    if !value.ends_with('Z')
        || !matches!(bytes.get(10), Some(b'T' | b't'))
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

fn actor_ref(value: &str) -> bool {
    value.len() <= 255
        && value.split_once(':').map_or_else(
            || is_slug(value),
            |(namespace, actor)| is_slug(namespace) && is_slug(actor),
        )
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value))
}

pub(super) fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidConfig, message)
}
