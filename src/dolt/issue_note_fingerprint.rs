//! Canonical v1 fingerprint for a normalized Issue note request.

use sha2::{Digest, Sha256};

pub(super) const KIND: &str = "issue.note";
pub(super) const MAX_BODY_BYTES: usize = 256 * 1024;

pub(super) struct NoteFingerprint<'a> {
    pub(super) workspace_id: &'a str,
    pub(super) area_id: &'a str,
    pub(super) issue_id: &'a str,
    pub(super) actor_ref: &'a str,
    pub(super) claim_id: Option<&'a str>,
    pub(super) expected_issue_revision: &'a str,
    pub(super) note_kind: &'a str,
}

impl NoteFingerprint<'_> {
    /// Pass the lowercase SHA-256 of the exact UTF-8 body bytes.
    pub(super) fn with_body_digest(&self, body_sha256: &str) -> String {
        let input = serde_json::json!([
            1,
            KIND,
            self.workspace_id,
            self.area_id,
            self.issue_id,
            self.actor_ref,
            self.claim_id,
            self.expected_issue_revision,
            self.note_kind,
            body_sha256
        ]);
        format!("{:x}", Sha256::digest(input.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_exact_utf8_and_uses_null_for_absent_claim() {
        let input = NoteFingerprint {
            workspace_id: "workspace",
            area_id: "area",
            issue_id: "issue",
            actor_ref: "quoted\"actor",
            claim_id: None,
            expected_issue_revision: "r1",
            note_kind: "progress",
        };
        let digest = format!("{:x}", Sha256::digest("café\r\n"));
        assert_eq!(
            input.with_body_digest(&digest),
            "5691c0ddf6312ba824147d0c1edb180b2963387082abc0bd05f0471d7bd93499"
        );
    }
}
