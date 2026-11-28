use serde::Deserialize;
use time::OffsetDateTime;

use super::Reason;
use super::json_object::{self, Object};
use super::reviewed_records::{AbsenceReview, canonical_utc};
use super::task_provenance::{BodyRange, Timestamp, fits_text_column};

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum ArchiveReview {
    Decision(#[serde(deserialize_with = "json_object::string_enum")] AbsenceReview),
    Evidence(Object<ArchiveEvidence>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArchiveEvidence {
    #[serde(deserialize_with = "json_object::deserialize")]
    archived_at: BodyRange,
    #[serde(deserialize_with = "json_object::deserialize")]
    archive_reason: BodyRange,
}

pub(crate) struct ArtifactArchive {
    pub(crate) archived_at: String,
    pub(crate) reason: String,
}

impl ArchiveReview {
    pub(super) fn prepare(
        &self,
        body: Option<&str>,
        updated: OffsetDateTime,
    ) -> Result<Option<ArtifactArchive>, Reason> {
        match self {
            Self::Decision(AbsenceReview::None) => Ok(None),
            Self::Decision(AbsenceReview::Unresolved) => Err(Reason::UnverifiedArtifactRetention),
            Self::Evidence(Object(evidence)) => {
                let invalid = Reason::UnverifiedArtifactRetention;
                let body = body.ok_or(invalid)?;
                let (_, archived) = Timestamp::select(Some(evidence.archived_at), body)
                    .filter(|(_, archived)| *archived >= updated)
                    .ok_or(invalid)?;
                let reason = evidence
                    .archive_reason
                    .select(body)
                    .filter(|reason| !reason.trim().is_empty() && fits_text_column(reason))
                    .ok_or(invalid)?;
                Ok(Some(ArtifactArchive {
                    archived_at: canonical_utc(archived),
                    reason: reason.to_owned(),
                }))
            }
        }
    }
}
