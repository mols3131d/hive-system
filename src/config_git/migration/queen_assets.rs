use std::collections::{BTreeMap, BTreeSet};

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    AssetObjectReader, Inventory, InventoryEntry, is_portable_bot_memory_locator, snapshot_object,
};
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
    mut on_candidate: impl FnMut(&str, &str, &str, Vec<u8>),
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
                if is_portable_bot_memory_locator(relative, None)
                    && slug_counts[relative] == 1
                    && !soul_aliases.contains(relative)
                    && let Some((soul, size)) =
                        soul.and_then(|soul| soul.size.map(|size| (soul, size)))
                    && size <= MAX_FILE_BYTES
                    && reviewed.get(relative).map(Vec::as_slice) == Some(&[soul.oid.as_str()])
                {
                    let bytes = snapshot_object(&|| reader.command(), "blob", &soul.oid, size)?;
                    if valid_soul(&bytes, relative) {
                        let source_raw_sha256 = digest(&bytes);
                        on_candidate(relative, &soul.oid, &source_raw_sha256, bytes);
                        outcome = Outcome::Candidate {
                            slug: relative.into(),
                            target_path: format!("assets/{soul_path}"),
                            source_raw_sha256,
                        };
                    }
                }
                report.bots_soul.push(record(&soul_path, soul, outcome));
            } else if matches!(relative, "README.md" | "AGENTS.md") {
                report.bots_soul.push(record(
                    path,
                    Some(entry),
                    Outcome::Excluded {
                        reason: Reason::ManagementOnly,
                    },
                ));
            }
            continue;
        };
        let Some(size) = entry.size else { continue };
        let leaf = child.rsplit('/').next().unwrap_or(child);
        if child == "README.md" || leaf == "AGENTS.md" {
            report.bots_soul.push(record(
                path,
                Some(entry),
                Outcome::Excluded {
                    reason: Reason::ManagementOnly,
                },
            ));
        } else if child == "MEMORY.md" || child.starts_with("memory/") {
            let navigation = child == "MEMORY.md"
                && is_portable_bot_memory_locator(id, None)
                && size <= MAX_FILE_BYTES
                && navigation_only(
                    &snapshot_object(&|| reader.command(), "blob", &entry.oid, size)?,
                    id,
                    inventory,
                );
            if navigation {
                report.navigation_only.push(record(
                    path,
                    Some(entry),
                    Outcome::Excluded {
                        reason: Reason::NavigationOnly,
                    },
                ));
            } else {
                report.bot_memories_historical.push(record(
                    path,
                    Some(entry),
                    Outcome::Held {
                        reason: Reason::CuratedMemoryDeferred,
                    },
                ));
            }
        }
    }
    Ok(report)
}

fn record<'a>(path: &str, entry: Option<&'a InventoryEntry>, outcome: Outcome) -> Record<'a> {
    Record {
        source_path: portable_bot_path(path).then(|| path.to_owned()),
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

fn portable_bot_path(path: &str) -> bool {
    let Some(relative) = path.strip_prefix("bots/") else {
        return false;
    };
    let (directories, file) = relative.rsplit_once('/').unwrap_or(("", relative));
    if !directories.is_empty() {
        let mut components = directories.split('/').peekable();
        let bot = components.next().unwrap_or_default();
        let memory_scope = if components.peek() == Some(&"memory") {
            components.next();
            components.next()
        } else {
            None
        };
        if !is_portable_bot_memory_locator(bot, memory_scope)
            || !components.all(|component| is_portable_bot_memory_locator(component, None))
        {
            return false;
        }
    }
    let stem = file.split('.').next().unwrap_or_default();
    // Device stems are at most four bytes. File stems can exceed the Bot slug limit.
    matches!(file, "SOUL.md" | "README.md" | "AGENTS.md" | "MEMORY.md")
        || (file.len() <= 255
            && file.split('.').all(is_slug)
            && (stem.len() > 4 || is_portable_bot_memory_locator(stem, None)))
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

fn split_document(bytes: &[u8]) -> Option<(&str, &str)> {
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
            return Some((&input[..offset], &input[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn valid_soul(bytes: &[u8], slug: &str) -> bool {
    let Some((header, body)) = split_document(bytes) else {
        return false;
    };
    let Ok(fields) = parse_v2::<CommonFields>(header.as_bytes(), "v1 Bot frontmatter") else {
        return false;
    };
    let valid_tags = fields.tags.is_none_or(|tags| {
        let unique = tags.iter().collect::<BTreeSet<_>>();
        !tags.is_empty()
            && unique.len() == tags.len()
            && tags
                .iter()
                .all(|tag| tag.split(':').all(|segment| !segment.trim().is_empty()))
    });
    if fields.description.trim().is_empty()
        || fields.title.is_some_and(|title| title.trim().is_empty())
        || !valid_tags
    {
        return false;
    }
    let mut offset = 0;
    let mut lines = body
        .split_inclusive('\n')
        .map(|line| {
            offset += line.len();
            (offset, line.trim_end_matches(['\r', '\n']))
        })
        .filter(|(_, line)| !line.trim().is_empty());
    let Some((_, title_line)) = lines.next().filter(|(_, line)| line.starts_with("# ")) else {
        return false;
    };
    let mut title = String::new();
    for event in Parser::new(title_line) {
        if let Event::Text(text) | Event::Code(text) = event {
            title.push_str(&text);
        }
    }
    if title.trim().is_empty() || !lines.next().is_some_and(|(_, line)| line == "## Identity") {
        return false;
    }
    let mut fields = BTreeMap::new();
    let mut purpose = None;
    for (end, line) in lines {
        if line == "## Purpose" {
            purpose = Some(&body[end..]);
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
    let Some(purpose) = purpose else {
        return false;
    };
    fields.len() == 3
        && fields.get("ID") == Some(&slug)
        && fields.get("Layer") == Some(&"1 — Manager")
        && fields.get("Name").is_some_and(|name| {
            !name.trim().is_empty()
                && name.chars().count() <= 255
                && !name.chars().any(char::is_control)
                && (title.trim() == name.trim() || !identity_declaration(&title, true))
        })
        && purpose
            .lines()
            .find(|line| !line.trim().is_empty())
            .is_some_and(|line| !line.trim_start().starts_with('#'))
        && !has_identity_declaration(purpose)
}

fn has_identity_declaration(markdown: &str) -> bool {
    let mut text = String::new();
    let mut heading = false;
    let mut items = 0;
    let mut leading_format_depth = 0;
    let mut formatted_label = false;
    for event in Parser::new(markdown) {
        match event {
            Event::Text(value) | Event::Code(value) => {
                if formatted_label && value.starts_with(char::is_whitespace) {
                    return true;
                }
                formatted_label = false;
                text.push_str(&value);
                continue;
            }
            Event::Start(Tag::Strong | Tag::Emphasis)
                if items > 0 && (text.trim().is_empty() || leading_format_depth > 0) =>
            {
                leading_format_depth += 1;
                continue;
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis) if leading_format_depth > 0 => {
                leading_format_depth -= 1;
                if leading_format_depth == 0 {
                    formatted_label = identity_declaration(&text, false);
                }
                continue;
            }
            Event::SoftBreak | Event::HardBreak if heading || leading_format_depth > 0 => {
                text.push(' ');
                continue;
            }
            Event::Start(Tag::Heading { .. } | Tag::Item | Tag::Paragraph)
            | Event::End(TagEnd::Heading(_) | TagEnd::Item | TagEnd::Paragraph)
            | Event::SoftBreak
            | Event::HardBreak => {}
            _ => continue,
        }
        if identity_declaration(&text, heading) {
            return true;
        }
        text.clear();
        formatted_label = false;
        match event {
            Event::Start(Tag::Heading { .. }) => heading = true,
            Event::End(TagEnd::Heading(_)) => heading = false,
            Event::Start(Tag::Item) => items += 1,
            Event::End(TagEnd::Item) => items -= 1,
            _ => {}
        }
    }
    identity_declaration(&text, heading)
}

fn identity_declaration(text: &str, heading: bool) -> bool {
    let label_text = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let label_text = label_text.trim_start_matches(['*', '_', '`']).trim_start();
    [
        "id",
        "bot id",
        "bot-id",
        "bot_id",
        "slug",
        "name",
        "display name",
        "layer",
        "role",
        "identity",
        "purpose",
    ]
    .into_iter()
    .any(|field| {
        label_text.strip_prefix(field).is_some_and(|rest| {
            let rest = rest.trim_start_matches(['*', '_', '`']);
            let value = rest.trim_start();
            value.is_empty()
                || value.starts_with([':', '=', '–', '—'])
                || value == "-"
                || value.starts_with("- ")
                || (heading && rest.starts_with(' '))
        })
    })
}

fn navigation_only(bytes: &[u8], bot: &str, inventory: &Inventory) -> bool {
    let Some((header, body)) = split_document(bytes) else {
        return false;
    };
    // Match the complete header so comments and free-form metadata remain held.
    if !matches!(
        header.trim_matches(['\r', '\n']),
        "description: \"Select memory navigation.\""
            | "description: \"Memory navigation.\""
            | "description: 'Select memory navigation.'"
            | "description: 'Memory navigation.'"
    ) {
        return false;
    }
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
        if !is_portable_bot_memory_locator(bot, Some(scope))
            || (label != scope
                && label.replace(' ', "-").to_ascii_lowercase() != scope.trim_start_matches('_'))
            || !links.insert(target)
            || !inventory
                .entries
                .get(&format!("bots/{bot}/{target}"))
                .is_some_and(|entry| !entry.is_directory())
        {
            return false;
        }
    }
    !links.is_empty()
}
