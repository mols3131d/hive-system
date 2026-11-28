use std::rc::Rc;

use serde::{Serialize, Serializer};
use serde_json::{Value, json};

use super::artifact_archive::ArtifactArchive;
use super::reviewed_records::sha256;
pub(crate) use super::work_artifact::IssueLink;
use super::work_artifact::WorkScope;

pub(crate) use super::directive_artifact_intake::{ArtifactKind, DirectiveStatus};

pub(crate) struct Intake {
    pub(crate) report: Value,
    pub(crate) records: Vec<PreparedRecord>,
}

pub(crate) struct PreparedRecord {
    pub(crate) id: String,
    pub(crate) content: Rc<Content>,
    pub(crate) fields: Fields,
}

pub(crate) struct Content {
    pub(crate) title: String,
    pub(crate) body: String,
}

pub(crate) enum Fields {
    Directive(DirectiveFields),
    Artifact(ArtifactFields),
}

pub(crate) struct DirectiveFields {
    pub(crate) status: DirectiveStatus,
    pub(crate) area_id: String,
    pub(crate) issuer_ref: String,
    pub(crate) source_ref: String,
    pub(crate) approval_ref: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) valid_from: Option<String>,
    pub(crate) expires_at: Option<String>,
    pub(crate) supersedes_id: Option<String>,
}

pub(crate) struct ArtifactFields {
    pub(crate) kind: ArtifactKind,
    pub(crate) author_ref: String,
    pub(crate) source_ref: String,
    pub(crate) created_at: String,
    pub(crate) substantive_updated_at: String,
    pub(crate) archive: Option<ArtifactArchive>,
    pub(crate) work: Option<Box<WorkScope>>,
}

// Only the portable evidence is serializable. The content and raw references
// are private to the callback that observed the verified Git snapshot.
impl Serialize for Fields {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let evidence = match self {
            Self::Directive(row) => {
                let mut evidence = json!({
                    "source_ref_sha256": sha256(&row.source_ref),
                    "issuer_ref_sha256": sha256(&row.issuer_ref),
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                    "supersedes_id": row.supersedes_id,
                    "valid_from": row.valid_from,
                    "expires_at": row.expires_at,
                });
                if let Some(approval) = &row.approval_ref {
                    evidence["approval_ref_sha256"] = json!(sha256(approval));
                } else {
                    evidence["approval_ref"] = Value::Null;
                }
                evidence
            }
            Self::Artifact(row) => {
                let mut evidence = json!({
                "source_ref_sha256": sha256(&row.source_ref),
                "author_ref_sha256": sha256(&row.author_ref),
                "created_at": row.created_at,
                "substantive_updated_at": row.substantive_updated_at,
                "scope": "standalone",
                });
                if let Some(archive) = &row.archive {
                    evidence["archived_at"] = json!(archive.archived_at);
                    evidence["archive_reason_sha256"] = json!(sha256(&archive.reason));
                } else {
                    evidence["archived_at"] = Value::Null;
                    evidence["archive_reason"] = Value::Null;
                }
                if let Some(work) = &row.work {
                    evidence["scope"] = json!("work_scoped");
                    evidence["issue_link"] = json!(work.issue_link);
                    evidence["created_at"] = json!(work.created_at);
                    evidence["substantive_updated_at"] = json!(work.substantive_updated_at);
                    if let Some(retention) = &work.retention {
                        evidence["retention"] = json!(retention);
                    }
                }
                evidence
            }
        };
        evidence.serialize(serializer)
    }
}
