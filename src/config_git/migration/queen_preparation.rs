use std::collections::HashMap;

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::is_portable_bot_memory_locator;
use crate::config_git::is_full_lowercase_oid;
use crate::dolt::is_uuid_v7;
use crate::error::{ErrorCode, HiveError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueenMapping {
    slug: String,
    source_git_blob_sha: String,
    source_raw_sha256: String,
    bot_id: String,
    display_name: String,
}

impl QueenMapping {
    pub(super) fn optional_list<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<Option<Vec<Self>>, D::Error> {
        Vec::deserialize(de).map(Some)
    }

    fn valid(&self) -> bool {
        is_portable_bot_memory_locator(&self.slug, None)
            && is_full_lowercase_oid(&self.source_git_blob_sha)
            && self.source_raw_sha256.len() == 64
            && self
                .source_raw_sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            && is_uuid_v7(&self.bot_id)
            && !self.display_name.trim().is_empty()
            && self.display_name.chars().count() <= 255
            && !self.display_name.chars().any(char::is_control)
    }
}

pub(crate) struct PreparedQueen {
    pub(crate) registry: QueenRegistry,
    source_git_blob_sha: String,
    pub(crate) source_raw_sha256: String,
    pub(crate) soul_bytes: Vec<u8>,
}

pub(crate) struct QueenRegistry {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) display_name: String,
}

impl PreparedQueen {
    fn report(&self) -> Value {
        json!({
            "status": "prepared",
            "slug": self.registry.slug,
            "bot_id": self.registry.id,
            "source_git_blob_sha": self.source_git_blob_sha,
            "source_raw_sha256": self.source_raw_sha256,
        })
    }
}

pub(super) struct Preparation<'a> {
    mappings: &'a [QueenMapping],
    eligible: HashMap<&'a str, usize>,
    prepared: Vec<Option<PreparedQueen>>,
}

impl<'a> Preparation<'a> {
    pub(super) fn new(mappings: &'a [QueenMapping]) -> Result<Self, HiveError> {
        let mut slug_counts = HashMap::new();
        let mut id_counts = HashMap::new();
        for mapping in mappings {
            if !mapping.valid() {
                return Err(HiveError::new(
                    ErrorCode::InvalidConfig,
                    "A Queen preparation mapping has invalid identity or digest fields.",
                ));
            }
            *slug_counts.entry(mapping.slug.as_str()).or_insert(0) += 1;
            *id_counts.entry(mapping.bot_id.as_str()).or_insert(0) += 1;
        }
        let eligible = mappings
            .iter()
            .enumerate()
            .filter(|(_, mapping)| {
                slug_counts[mapping.slug.as_str()] == 1 && id_counts[mapping.bot_id.as_str()] == 1
            })
            .map(|(index, mapping)| (mapping.slug.as_str(), index))
            .collect();
        Ok(Self {
            mappings,
            eligible,
            prepared: (0..mappings.len()).map(|_| None).collect(),
        })
    }

    pub(super) fn accept_candidate(&mut self, slug: &str, oid: &str, digest: &str, bytes: Vec<u8>) {
        let Some(&index) = self.eligible.get(slug) else {
            return;
        };
        let mapping = &self.mappings[index];
        if mapping.source_git_blob_sha == oid && mapping.source_raw_sha256 == digest {
            self.prepared[index] = Some(PreparedQueen {
                registry: QueenRegistry {
                    id: mapping.bot_id.clone(),
                    slug: slug.to_owned(),
                    display_name: mapping.display_name.clone(),
                },
                source_git_blob_sha: oid.to_owned(),
                source_raw_sha256: digest.to_owned(),
                soul_bytes: bytes,
            });
        }
    }

    pub(super) fn finish(self) -> (Value, Vec<PreparedQueen>) {
        let mut summary = Vec::with_capacity(self.mappings.len());
        let mut souls = Vec::new();
        for (mapping, prepared) in self.mappings.iter().zip(self.prepared) {
            if let Some(queen) = prepared {
                summary.push(queen.report());
                souls.push(queen);
            } else {
                summary.push(json!({
                    "status": "held",
                    "slug": mapping.slug,
                    "reason": "invalid_soul_or_slug",
                }));
            }
        }
        (Value::Array(summary), souls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_payload_keeps_exact_bytes_and_reviewed_registry_data() {
        let bytes = "---\r\ndescription: \"Synthetic café.\"\r\n---\r\n# Queen\r\n";
        let mappings = [QueenMapping {
            slug: "example-queen".into(),
            source_git_blob_sha: "2".repeat(40),
            source_raw_sha256: "3".repeat(64),
            bot_id: "018f0000-0000-7000-8000-000000000001".into(),
            display_name: "Reviewed synthetic display".into(),
        }];
        let mut preparation = Preparation::new(&mappings).unwrap();
        preparation.accept_candidate(
            "example-queen",
            &"2".repeat(40),
            &"3".repeat(64),
            bytes.as_bytes().to_vec(),
        );
        let (report, souls) = preparation.finish();
        assert_eq!(souls.len(), 1);
        let queen = &souls[0];
        assert_eq!(queen.soul_bytes, bytes.as_bytes());
        assert_eq!(queen.registry.display_name, mappings[0].display_name);
        assert_eq!(queen.registry.slug, mappings[0].slug);
        assert_eq!(queen.registry.id, mappings[0].bot_id);
        assert_eq!(report[0]["status"], "prepared");
        assert!(!report.to_string().contains("Synthetic café"));
        assert!(!report.to_string().contains(&mappings[0].display_name));
    }
}
