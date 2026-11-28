use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{AssetObjectReader, Inventory, InventoryEntry, snapshot_object};
use crate::cli::is_slug;
use crate::config_file::{MAX_FILE_BYTES, parse_v2};
use crate::config_git::is_full_lowercase_oid;
use crate::error::{ErrorCode, HiveError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SoulReview {
    slug: String,
    source_git_blob_sha: String,
}

#[derive(Serialize)]
pub(super) struct QueenAssets<'a> {
    bots_soul: Vec<Record<'a>>,
    bot_memories_historical: Vec<Record<'a>>,
    navigation_only: Vec<Record<'a>>,
}

#[derive(Serialize)]
struct Record<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    source_path_sha256: String,
    source_git_blob_sha: Option<&'a str>,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Candidate {
        slug: String,
        target_path: String,
        source_raw_sha256: String,
    },
    Held {
        reason: Reason,
    },
    Excluded {
        reason: Reason,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    InvalidSoulOrSlug,
    CuratedMemoryDeferred,
    ManagementOnly,
    NavigationOnly,
}

pub(super) fn classify<'a>(
    inventory: &'a Inventory,
    reader: &AssetObjectReader<'_>,
    reviews: &[SoulReview],
) -> Result<QueenAssets<'a>, HiveError> {
    let entries = &inventory.entries;
    let mut slug_counts = BTreeMap::<String, usize>::new();
    let mut soul_aliases = BTreeSet::new();
    for path in entries.range("bots/".to_owned()..).map(|(path, _)| path) {
        let Some(relative) = path.strip_prefix("bots/") else {
            break;
        };
        if let Some((id, child)) = relative.split_once('/') {
            if child != "SOUL.md" && child.eq_ignore_ascii_case("SOUL.md") {
                soul_aliases.insert(id);
            }
        } else {
            let id = relative;
            *slug_counts.entry(id.to_ascii_lowercase()).or_default() += 1;
        }
    }

    let mut reviewed = BTreeMap::<&str, Vec<&str>>::new();
    for review in reviews {
        if !valid_slug(&review.slug)
            || !is_full_lowercase_oid(&review.source_git_blob_sha)
            || !entries
                .get(&format!("bots/{}", review.slug))
                .is_some_and(InventoryEntry::is_directory)
        {
            return Err(HiveError::new(
                ErrorCode::InvalidConfig,
                "Queen reviews must identify an inventoried Bot directory and a full lowercase Git blob ID.",
            ));
        }
        reviewed
            .entry(&review.slug)
            .or_default()
            .push(&review.source_git_blob_sha);
    }

    let mut report = QueenAssets {
        bots_soul: Vec::new(),
        bot_memories_historical: Vec::new(),
        navigation_only: Vec::new(),
    };
    for (path, entry) in entries.range("bots/".to_owned()..) {
        let Some(relative) = path.strip_prefix("bots/") else {
            break;
        };
        let Some((id, child)) = relative.split_once('/') else {
            if entry.is_directory() {
                let soul_path = format!("{path}/SOUL.md");
                let soul = entries.get(&soul_path);
                let mut outcome = Outcome::Held {
                    reason: Reason::InvalidSoulOrSlug,
                };
                if valid_slug(relative)
                    && slug_counts[relative] == 1
                    && !soul_aliases.contains(relative)
                    && let Some((soul, size)) =
                        soul.and_then(|soul| soul.size.map(|size| (soul, size)))
                    && size <= MAX_FILE_BYTES
                    && reviewed.get(relative).map(Vec::as_slice) == Some(&[soul.oid.as_str()])
                {
                    let bytes = snapshot_object(&|| reader.command(), "blob", &soul.oid, size)?;
                    if valid_soul(&bytes, relative) {
                        outcome = Outcome::Candidate {
                            slug: relative.into(),
                            target_path: format!("assets/{soul_path}"),
                            source_raw_sha256: digest(&bytes),
                        };
                    }
                }
                report
                    .bots_soul
                    .push(record(&soul_path, soul, valid_slug(relative), outcome));
            } else if matches!(relative, "README.md" | "AGENTS.md") {
                report.bots_soul.push(record(
                    path,
                    Some(entry),
                    true,
                    Outcome::Excluded {
                        reason: Reason::ManagementOnly,
                    },
                ));
            }
            continue;
        };
        let Some(size) = entry.size else { continue };
        let safe_id = valid_slug(id);
        let leaf = child.rsplit('/').next().unwrap_or(child);
        if child == "README.md" || leaf == "AGENTS.md" {
            report.bots_soul.push(record(
                path,
                Some(entry),
                safe_id,
                Outcome::Excluded {
                    reason: Reason::ManagementOnly,
                },
            ));
        } else if child == "MEMORY.md" || child.starts_with("memory/") {
            let navigation = child == "MEMORY.md"
                && safe_id
                && size <= MAX_FILE_BYTES
                && navigation_only(
                    &snapshot_object(&|| reader.command(), "blob", &entry.oid, size)?,
                    &format!("bots/{id}"),
                    inventory,
                );
            if navigation {
                report.navigation_only.push(record(
                    path,
                    Some(entry),
                    true,
                    Outcome::Excluded {
                        reason: Reason::NavigationOnly,
                    },
                ));
            } else {
                report.bot_memories_historical.push(record(
                    path,
                    Some(entry),
                    safe_id,
                    Outcome::Held {
                        reason: Reason::CuratedMemoryDeferred,
                    },
                ));
            }
        }
    }
    Ok(report)
}

fn record<'a>(
    path: &str,
    entry: Option<&'a InventoryEntry>,
    safe_id: bool,
    outcome: Outcome,
) -> Record<'a> {
    Record {
        source_path: safe_id.then(|| path.to_owned()),
        source_path_sha256: digest(path.as_bytes()),
        source_git_blob_sha: entry
            .filter(|entry| !entry.is_directory())
            .map(|entry| entry.oid.as_str()),
        outcome,
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_slug(slug: &str) -> bool {
    slug.len() <= 100 && is_slug(slug)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommonFields {
    description: String,
    #[serde(default, deserialize_with = "present")]
    title: Option<String>,
    #[serde(default, deserialize_with = "present")]
    tags: Option<Vec<String>>,
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    de: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(de).map(Some)
}

fn document_body(bytes: &[u8]) -> Option<&str> {
    let input = std::str::from_utf8(bytes).ok()?;
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    if input.contains('\0') {
        return None;
    }
    let input = input
        .strip_prefix("---\n")
        .or_else(|| input.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in input.split_inclusive('\n') {
        if matches!(line, "---\n" | "---\r\n" | "---") {
            let fields: CommonFields =
                parse_v2(&input.as_bytes()[..offset], "v1 Bot frontmatter").ok()?;
            let valid_tags = fields.tags.is_none_or(|tags| {
                let unique = tags.iter().collect::<BTreeSet<_>>();
                !tags.is_empty()
                    && unique.len() == tags.len()
                    && tags
                        .iter()
                        .all(|tag| tag.split(':').all(|segment| !segment.trim().is_empty()))
            });
            return (!fields.description.trim().is_empty()
                && fields.title.is_none_or(|title| !title.trim().is_empty())
                && valid_tags)
                .then(|| &input[offset + line.len()..]);
        }
        offset += line.len();
    }
    None
}

fn valid_soul(bytes: &[u8], slug: &str) -> bool {
    let Some(body) = document_body(bytes) else {
        return false;
    };
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    if !lines.next().is_some_and(|line| {
        line.strip_prefix("# ")
            .is_some_and(|heading| !heading.trim().is_empty())
    }) || lines.next() != Some("## Identity")
    {
        return false;
    }
    let mut fields = BTreeMap::new();
    let mut purpose = false;
    for line in lines.by_ref() {
        if line == "## Purpose" {
            purpose = true;
            break;
        }
        let Some((key, value)) = line
            .strip_prefix("- ")
            .and_then(|line| line.split_once(": "))
        else {
            return false;
        };
        if fields.insert(key, value).is_some() {
            return false;
        }
    }
    let purpose_text = lines.next();
    purpose
        && fields.len() == 3
        && fields.get("ID") == Some(&slug)
        && fields.get("Layer") == Some(&"1 — Manager")
        && fields.get("Name").is_some_and(|name| {
            !name.trim().is_empty()
                && name.chars().count() <= 255
                && !name.chars().any(char::is_control)
        })
        && purpose_text.is_some_and(|line| !line.trim_start().starts_with('#'))
        && !purpose_text.into_iter().chain(lines).any(|line| {
            let line = line.trim();
            matches!(line, "## Identity" | "## Purpose")
                || ["- ID:", "- Name:", "- Layer:", "- Role:"]
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
        })
}

fn navigation_only(bytes: &[u8], directory: &str, inventory: &Inventory) -> bool {
    let Some(body) = document_body(bytes) else {
        return false;
    };
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    if !matches!(lines.next(), Some("# Memory" | "# Memory Navigation")) {
        return false;
    }
    let mut links = BTreeSet::new();
    for line in lines {
        let Some((label, target)) = line
            .strip_prefix("- [")
            .and_then(|line| line.strip_suffix(')'))
            .and_then(|line| line.split_once("]("))
        else {
            return false;
        };
        let Some(scope) = target
            .strip_prefix("memory/")
            .and_then(|target| target.strip_suffix("/MEMORY.md"))
        else {
            return false;
        };
        if !(valid_slug(scope) || matches!(scope, "_general" | "_hive-userdata"))
            || (label != scope
                && label.replace(' ', "-").to_ascii_lowercase() != scope.trim_start_matches('_'))
            || !links.insert(target)
            || !inventory
                .entries
                .get(&format!("{directory}/{target}"))
                .is_some_and(|entry| !entry.is_directory())
        {
            return false;
        }
    }
    !links.is_empty()
}
