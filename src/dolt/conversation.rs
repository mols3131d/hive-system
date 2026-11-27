use std::ffi::OsStr;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serde_saphyr::Tagged;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{DoltFailure, DoltSuccess, Snapshot, is_uuid, is_uuid_v7, unverified_authority};
use crate::cli::{CommonOptions, ConversationShowArgs, is_slug};
use crate::config_file::{MAX_FILE_BYTES, parse_v2, present, untagged};
use crate::config_v2;
use crate::error::{ErrorCode, HiveError};

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
// Two delimiter lines, each at most five bytes including CRLF.
const MAX_CONVERSATION_BYTES: usize = MAX_FILE_BYTES + MAX_BODY_BYTES + 10;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    #[serde(deserialize_with = "untagged")]
    version: Value,
    #[serde(deserialize_with = "untagged")]
    workspace_id: String,
    #[serde(deserialize_with = "untagged")]
    bot_id: String,
    #[serde(deserialize_with = "untagged")]
    bot_slug: String,
    #[serde(deserialize_with = "untagged")]
    conversation_id: String,
    #[serde(deserialize_with = "untagged")]
    imported_at: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    title: Option<String>,
    #[serde(deserialize_with = "untagged")]
    provenance: Provenance,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    #[serde(deserialize_with = "untagged")]
    source: Source,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    session_ref: Option<String>,
    #[serde(deserialize_with = "untagged")]
    body_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    File,
    Stdin,
}

pub(crate) fn show(
    common: &CommonOptions,
    args: &ConversationShowArgs,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    read(common, args).map_err(DoltFailure::without_context)
}

fn read(common: &CommonOptions, args: &ConversationShowArgs) -> Result<DoltSuccess, HiveError> {
    let bot_id = selector(args.bot.as_deref(), "--bot")?;
    let conversation_id = selector(args.id.as_deref(), "--id")?;
    let expected_id = args
        .target
        .workspace_id
        .as_deref()
        .filter(|id| is_uuid(id))
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidArgument,
                "Supply --workspace-id as a canonical lowercase UUID.",
            )
        })?;
    let workspace = common.workspace.as_deref().ok_or_else(|| {
        HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply an explicit --workspace.",
        )
    })?;
    let candidate = config_v2::read_explicit(workspace)?;
    let workspace_id = candidate.workspace_id();
    if expected_id != workspace_id {
        return Err(unverified_authority(
            "The workspace descriptor identity does not match the expected UUID.",
        ));
    }
    // The explicit root and endpoint are paired by portable identity for
    // inspection. This does not establish which physical db/ serves the endpoint.
    let (mut snapshot, _) = Snapshot::open(&args.target)?;
    let queens: Vec<(String, String)> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT id, slug FROM bots AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
            )
        },
        (bot_id,),
    )?;
    let bot_slug = match queens.as_slice() {
        [(id, slug)] if id == bot_id && is_slug(slug) => slug,
        [] => {
            return Err(HiveError::new(
                ErrorCode::NotFound,
                "The Queen is absent from the committed registry.",
            ));
        }
        _ => {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "The committed Queen identity is invalid.",
            ));
        }
    };
    let filename = format!("{conversation_id}.md");
    let components = ["conversations", "bots", bot_id, &filename].map(OsStr::new);
    let bytes = candidate
        .root
        .read_bounded(&components, MAX_CONVERSATION_BYTES)?
        .ok_or_else(|| {
            HiveError::new(ErrorCode::NotFound, "The selected conversation is missing.")
        })?;
    let (metadata, body) = validate_file(&bytes, workspace_id, bot_id, bot_slug, conversation_id)?;
    Ok(DoltSuccess {
        context: json!({
            "workspace_id": workspace_id,
            "db_commit": snapshot.commit,
            "policy_revision": candidate.policy_revision(),
            "scope": "conversation",
        }),
        data: json!({ "metadata": metadata, "body": body }),
    })
}

fn selector<'a>(value: Option<&'a OsStr>, flag: &str) -> Result<&'a str, HiveError> {
    value
        .and_then(OsStr::to_str)
        .filter(|id| is_uuid_v7(id))
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidArgument,
                format!("Supply {flag} as a canonical lowercase UUIDv7."),
            )
        })
}

fn validate_file<'a>(
    bytes: &'a [u8],
    workspace_id: &str,
    bot_id: &str,
    bot_slug: &str,
    conversation_id: &str,
) -> Result<(Metadata, &'a str), HiveError> {
    let input = str::from_utf8(bytes)
        .ok()
        .filter(|input| !input.contains('\0'))
        .ok_or_else(|| invalid("The conversation must be UTF-8 without NUL bytes."))?;
    let (header, body) = split_frontmatter(input)?;
    if body.len() > MAX_BODY_BYTES {
        return Err(invalid(
            "The conversation body exceeds the 4 MiB size limit.",
        ));
    }

    let Tagged(metadata, None) =
        parse_v2::<Tagged<Metadata>>(header.as_bytes(), "Conversation frontmatter").map_err(
            |_| invalid("The conversation frontmatter contains invalid YAML or fields."),
        )?
    else {
        return Err(invalid("The conversation frontmatter must be untagged."));
    };
    if metadata.version.as_u64() != Some(1) {
        return Err(invalid("The conversation frontmatter version must be 1."));
    }
    if metadata.workspace_id != workspace_id
        || metadata.bot_id != bot_id
        || metadata.bot_slug != bot_slug
        || metadata.conversation_id != conversation_id
    {
        return Err(invalid(
            "The conversation frontmatter identity does not match its selection.",
        ));
    }
    if !OffsetDateTime::parse(&metadata.imported_at, &Rfc3339)
        .is_ok_and(|time| time.offset().is_utc())
        || metadata.imported_at.ends_with("-00:00")
        || metadata.title.as_deref().is_some_and(|title| {
            title.is_empty() || !valid_label(title) || title.contains(['\u{2028}', '\u{2029}'])
        })
        || metadata
            .provenance
            .session_ref
            .as_deref()
            .is_some_and(|reference| !valid_label(reference))
    {
        return Err(invalid("The conversation frontmatter metadata is invalid."));
    }
    // Its lowercase hexadecimal encoding also verifies the digest field's shape.
    if metadata.provenance.body_sha256 != format!("{:x}", Sha256::digest(body.as_bytes())) {
        return Err(invalid("The conversation body digest does not match."));
    }
    Ok((metadata, body))
}

fn split_frontmatter(input: &str) -> Result<(&str, &str), HiveError> {
    let delimiter = |line: &str| matches!(line, "---\n" | "---\r\n");
    let mut lines = input.split_inclusive('\n');
    if let Some(opening) = lines.next().filter(|line| delimiter(line)) {
        let start = opening.len();
        let mut end = start;
        for line in lines {
            if delimiter(line) {
                return Ok((&input[start..end], &input[end + line.len()..]));
            }
            end += line.len();
            if end - start > MAX_FILE_BYTES {
                return Err(invalid(
                    "The conversation frontmatter exceeds the 64 KiB size limit.",
                ));
            }
        }
    }
    Err(invalid(
        "The conversation frontmatter delimiters are invalid.",
    ))
}

fn valid_label(value: &str) -> bool {
    value.chars().count() <= 255 && !value.chars().any(char::is_control)
}

fn invalid(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::InvalidRecord, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "01900000-0000-7000-8000-000000000001";
    const BOT: &str = "01900000-0000-7000-8000-000000000002";
    const CONVERSATION: &str = "01900000-0000-7000-8000-000000000003";
    const SLUG: &str = "queen-fixture";

    fn fixture(body: &str, newline: &str) -> String {
        let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
        [
            "---".to_owned(),
            "version: 1".to_owned(),
            format!("workspace_id: {WORKSPACE}"),
            format!("bot_id: {BOT}"),
            format!("bot_slug: {SLUG}"),
            format!("conversation_id: {CONVERSATION}"),
            "imported_at: \"2026-11-27T09:00:00Z\"".to_owned(),
            "title: Synthetic title".to_owned(),
            "provenance:".to_owned(),
            "  source: stdin".to_owned(),
            format!("  body_sha256: {digest}"),
            "---".to_owned(),
            body.to_owned(),
        ]
        .join(newline)
    }

    fn parse(bytes: &[u8]) -> Result<(Metadata, &str), HiveError> {
        validate_file(bytes, WORKSPACE, BOT, SLUG, CONVERSATION)
    }

    #[test]
    fn conversation_show_preserves_body_bytes() {
        for body in ["", "\r\n\n# 기록\r\n---\nno final newline"] {
            for newline in ["\n", "\r\n"] {
                let file = fixture(body, newline);
                let (metadata, actual) = parse(file.as_bytes()).unwrap();
                assert_eq!(actual.as_bytes(), body.as_bytes());
                assert_eq!(metadata.provenance.session_ref, None);
            }
        }
    }

    #[test]
    fn conversation_show_rejects_invalid_frontmatter_and_tampering_without_leaks() {
        let valid = fixture("private-fixture-body", "\n");
        for (from, to) in [
            ("Synthetic title", "null"),
            ("Synthetic title", "true"),
            ("Synthetic title", "\"\\u2028\""),
            ("Synthetic title", "&private-anchor private-title"),
            ("Synthetic title", "!!str private-tagged-value"),
            ("Synthetic title", "private-title\ntitle: duplicate"),
            ("Synthetic title", "private-title\nprivate-unknown: value"),
            ("version: 1", "!!map\nversion: 1"),
            ("version: 1", "version: '1'"),
            ("version: 1", "version: 2"),
            ("source: stdin", "source: private-source"),
            ("2026-11-27T09:00:00Z", "2026-02-30T09:00:00Z"),
            ("2026-11-27T09:00:00Z", "2026-11-27T09:00:00-00:00"),
            ("2026-11-27T09:00:00Z", "2026-11-27T09:00:00+01:00"),
            ("private-fixture-body", "private-changed-body"),
            ("---\nprivate", "--- \nprivate"),
            (WORKSPACE, "private-wrong-id"),
            (BOT, "private-wrong-id"),
            (SLUG, "private-wrong-slug"),
            (CONVERSATION, "private-wrong-id"),
        ] {
            let invalid = valid.replace(from, to);
            let Err(error) = parse(invalid.as_bytes()) else {
                panic!("Rejected fixture was accepted for {from:?} -> {to:?}");
            };
            assert!(!error.to_string().contains("private"));
        }
    }

    #[test]
    fn conversation_show_rejects_invalid_utf8_nul_and_oversize_input() {
        assert!(parse(&[0xff]).is_err());
        assert!(parse(fixture("before\0after", "\n").as_bytes()).is_err());
        let body = "a".repeat(MAX_BODY_BYTES);
        assert!(parse(fixture(&body, "\n").as_bytes()).is_ok());
        assert!(parse(fixture(&format!("{body}b"), "\n").as_bytes()).is_err());

        let valid = fixture("body", "\n");
        let (header, _) = split_frontmatter(&valid).unwrap();
        let comment = format!("#{}\n", "x".repeat(MAX_FILE_BYTES - header.len() - 2));
        let limit = valid.replacen("version: 1", &format!("{comment}version: 1"), 1);
        assert!(parse(limit.as_bytes()).is_ok());
        let oversize = limit.replacen("#x", "#xx", 1);
        assert!(parse(oversize.as_bytes()).is_err());
    }
}
