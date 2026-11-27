use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use crate::cli::{DoltTargetArgs, is_slug};
use crate::error::{ErrorCode, HiveError};

use super::{DoltFailure, DoltSuccess, Snapshot, encode_id_cursor, id_cursor_key, is_uuid_v7};

const COLUMNS: &str =
    "id, slug, display_name, revision, DATE_FORMAT(retired_at, '%Y-%m-%dT%H:%i:%s.%fZ')";
const SORT: &str = "CAST(id AS BINARY) ASC";

type BotRow = (String, String, String, String, Option<String>);

#[derive(Serialize)]
pub(super) struct BotMetadata {
    pub(super) id: String,
    pub(super) slug: String,
    display_name: String,
    pub(super) revision: String,
    role: &'static str,
    status: &'static str,
    retired_at: Option<String>,
}

impl TryFrom<BotRow> for BotMetadata {
    type Error = HiveError;

    fn try_from((id, slug, display_name, revision, retired_at): BotRow) -> Result<Self, HiveError> {
        if !is_uuid_v7(&id) || !is_slug(&slug) {
            return Err(HiveError::new(
                ErrorCode::InvalidRecord,
                "A selected Bot has an invalid UUIDv7 or slug at the verified Dolt commit.",
            ));
        }
        Ok(Self {
            id,
            slug,
            display_name,
            revision,
            role: "queen",
            status: if retired_at.is_some() {
                "retired"
            } else {
                "active"
            },
            retired_at,
        })
    }
}

pub(crate) fn list(
    target: &DoltTargetArgs,
    status: Option<&str>,
    limit: u32,
    cursor: Option<&str>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let status = status.unwrap_or("active");
    let predicate = match status {
        "active" => "retired_at IS NULL AND",
        "retired" => "retired_at IS NOT NULL AND",
        "all" => "",
        _ => {
            return Err(invalid(
                "Use active, retired, or all for the Bot registry status.",
            ));
        }
    };
    if !(1..=100).contains(&limit) {
        return Err(invalid("Use a page size between 1 and 100."));
    }
    let cursor = cursor
        .map(|cursor| {
            id_cursor_key(cursor)
                .map(|id| (id, cursor))
                .ok_or_else(stale_cursor)
        })
        .transpose()
        .map_err(DoltFailure::without_context)?;

    read(target, |snapshot, workspace_id| {
        let binding = json!([
            "bot-list-v1",
            workspace_id,
            snapshot.commit,
            SORT,
            status,
            limit
        ]);
        let (comparison, after) = match cursor {
            Some((id, cursor)) if cursor == encode_id_cursor(id, &binding) => (">", id),
            Some(_) => return Err(stale_cursor()),
            None => (">=", ""),
        };
        let rows: Vec<BotRow> = snapshot.exec_rows(
            |commit| {
                format!(
                    "SELECT {COLUMNS} FROM bots AS OF '{commit}' \
                 WHERE {predicate} CAST(id AS BINARY) {comparison} ? ORDER BY {SORT} LIMIT ?"
                )
            },
            (after, limit + 1),
        )?;
        let mut items: Vec<_> = rows
            .into_iter()
            .map(BotMetadata::try_from)
            .collect::<Result<_, _>>()?;
        let truncated = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = items
            .last()
            .filter(|_| truncated)
            .map(|item| encode_id_cursor(&item.id, &binding));
        Ok(json!({
            "items": items,
            "next_cursor": next_cursor,
            "truncated": truncated,
            "observed_revision": snapshot.commit,
        }))
    })
}

pub(crate) fn show(
    target: &DoltTargetArgs,
    id: &str,
    soul_selection: Option<(&Path, &str)>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    if !is_uuid_v7(id) {
        return Err(invalid("Use an exact lowercase UUIDv7 for the Bot ID."));
    }
    let Some((root, commit)) = soul_selection else {
        return read(target, |snapshot, _| {
            Ok(json!({"bot": select_bot(snapshot, id)?, "observed_revision": snapshot.commit}))
        });
    };
    #[cfg(target_os = "linux")]
    {
        show_soul(target, id, root, commit)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, commit);
        Err(DoltFailure::without_context(HiveError::new(
            ErrorCode::NotImplemented,
            "Soul inspection requires Linux with file-descriptor access through /proc.",
        )))
    }
}

fn select_bot(snapshot: &mut Snapshot, id: &str) -> Result<BotMetadata, HiveError> {
    let rows: Vec<BotRow> = snapshot.exec_rows(
        |commit| {
            format!(
                "SELECT {COLUMNS} FROM bots AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 1"
            )
        },
        (id,),
    )?;
    let row = rows.into_iter().next().ok_or_else(|| {
        HiveError::new(
            ErrorCode::NotFound,
            "The selected Bot was not found at the verified Dolt commit.",
        )
    })?;
    BotMetadata::try_from(row)
}

#[cfg(target_os = "linux")]
pub(super) struct SoulRead {
    pub(super) bot: BotMetadata,
    pub(super) path: String,
    pub(super) body: String,
}

#[cfg(target_os = "linux")]
pub(super) fn read_soul(
    snapshot: &mut Snapshot,
    root: &crate::config_file::ScopedDirectory,
    id: &str,
    commit: &str,
) -> Result<SoulRead, HiveError> {
    let bot = select_bot(snapshot, id)?;
    let relative = format!("bots/{}/SOUL.md", bot.slug);
    let bytes = crate::config_git::read_asset_blob(root, commit, Path::new(&relative))?
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::NotFound,
                "The selected Queen's Soul is missing from the asset commit.",
            )
        })?;
    let body = soul_body(&bytes, &snapshot.workspace_id, &bot)?.to_owned();
    Ok(SoulRead {
        bot,
        path: format!("assets/{relative}"),
        body,
    })
}

#[cfg(target_os = "linux")]
fn show_soul(
    target: &DoltTargetArgs,
    id: &str,
    root: &Path,
    commit: &str,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    use crate::{config_git, config_v2};

    if !config_git::is_full_oid(commit) {
        return Err(invalid(
            "Supply a full Git commit object ID with --asset-commit.",
        ));
    }
    let candidate = config_v2::read_explicit(root).map_err(DoltFailure::without_context)?;
    let expected_id = target
        .workspace_id
        .as_deref()
        .filter(|id| super::is_uuid(id))
        .ok_or_else(|| invalid("Supply --workspace-id as a canonical lowercase UUID."))?;
    if candidate.workspace_id() != expected_id {
        return Err(DoltFailure::without_context(super::unverified_authority(
            "The workspace descriptor identity does not match the expected UUID.",
        )));
    }

    let mut result = read(target, |snapshot, _| {
        let soul = read_soul(snapshot, &candidate.root, id, commit)?;
        Ok(json!({
            "bot": soul.bot, "observed_revision": snapshot.commit,
            "soul": { "path": soul.path, "body": soul.body },
        }))
    })?;
    result.context["asset_commit"] = json!(commit.to_ascii_lowercase());
    result.context["policy_revision"] = json!(candidate.policy_revision());
    result.context["scope"] = json!("soul");
    Ok(result)
}

#[cfg(target_os = "linux")]
fn soul_body<'a>(
    bytes: &'a [u8],
    workspace_id: &str,
    bot: &BotMetadata,
) -> Result<&'a str, HiveError> {
    use crate::config_file::parse_v2;
    use serde_json::Map;

    let input = str::from_utf8(bytes)
        .ok()
        .filter(|input| !input.contains('\0'))
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidRecord,
                "The Soul must be UTF-8 without NUL bytes.",
            )
        })?;
    let contents = input.strip_prefix('\u{feff}').unwrap_or(input);
    let Some(contents) = contents
        .strip_prefix("---\n")
        .or_else(|| contents.strip_prefix("---\r\n"))
        .or_else(|| (contents == "---").then_some(""))
    else {
        return Ok(input);
    };
    let lines = contents.split_inclusive('\n');
    let mut offset = 0;
    for line in lines {
        if matches!(line, "---\n" | "---\r\n" | "---") {
            let header = &contents[..offset];
            if !header.trim().is_empty() {
                let fields: Map<String, Value> = parse_v2(header.as_bytes(), "Soul frontmatter")
                    .map_err(|_| {
                        HiveError::new(ErrorCode::InvalidRecord, "The Soul frontmatter is invalid.")
                    })?;
                for (field, expected) in [
                    ("workspace_id", workspace_id),
                    ("id", bot.id.as_str()),
                    ("bot_id", bot.id.as_str()),
                    ("slug", bot.slug.as_str()),
                    ("bot_slug", bot.slug.as_str()),
                    ("display_name", bot.display_name.as_str()),
                    ("role", bot.role),
                ] {
                    if fields
                        .get(field)
                        .is_some_and(|value| value.as_str() != Some(expected))
                    {
                        return Err(HiveError::new(
                            ErrorCode::InvalidRecord,
                            "The Soul frontmatter identity does not match the committed registry.",
                        ));
                    }
                }
            }
            return Ok(&contents[offset + line.len()..]);
        }
        offset += line.len();
    }
    Err(HiveError::new(
        ErrorCode::InvalidRecord,
        "The Soul frontmatter has no closing delimiter.",
    ))
}

fn read(
    target: &DoltTargetArgs,
    query: impl FnOnce(&mut Snapshot, &str) -> Result<Value, HiveError>,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let (mut snapshot, workspace_id) =
        Snapshot::open(target).map_err(DoltFailure::without_context)?;
    let context = json!({"workspace_id": workspace_id, "db_commit": snapshot.commit});
    match query(&mut snapshot, &workspace_id) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

fn invalid(message: &str) -> Box<DoltFailure> {
    DoltFailure::without_context(HiveError::new(ErrorCode::InvalidArgument, message))
}

fn stale_cursor() -> HiveError {
    HiveError::new(
        ErrorCode::StaleCursor,
        "The Bot list cursor is invalid for the selected workspace, commit, or query.",
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn validates_optional_soul_identity_and_returns_only_the_body() {
        let id = "01900000-0000-7000-8000-00000000000a";
        let workspace = "01900000-0000-7000-8000-000000000001";
        let bot = BotMetadata::try_from((
            id.into(),
            "queen-alpha".into(),
            "Synthetic Queen".into(),
            "revision".into(),
            None,
        ))
        .unwrap();
        let body = "# Synthetic Queen 기록\r\nKeep exact bytes.";
        let valid = format!(
            "---\r\nworkspace_id: {workspace}\r\nbot_id: {id}\r\nslug: queen-alpha\r\nrole: queen\r\n---\r\n{body}"
        );
        assert_eq!(soul_body(valid.as_bytes(), workspace, &bot).unwrap(), body);
        assert_eq!(soul_body(body.as_bytes(), workspace, &bot).unwrap(), body);
        for header in [
            "workspace_id: another-workspace",
            "bot_id: another-queen",
            "id: another-queen",
            "slug: another-queen",
            "bot_slug: another-queen",
            "display_name: another-queen",
            "role: overlord",
            "slug: null",
            "slug: queen-alpha\nslug: queen-alpha",
        ] {
            let input = format!("---\n{header}\n---\n{body}");
            assert!(matches!(
                soul_body(input.as_bytes(), workspace, &bot)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidRecord
            ));
        }
        assert!(soul_body(&[0xff], workspace, &bot).is_err());
        assert!(soul_body(b"---\nslug: queen-alpha\n", workspace, &bot).is_err());
    }
}
