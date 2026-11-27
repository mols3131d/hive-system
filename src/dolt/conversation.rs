#[cfg(any(target_os = "linux", target_os = "macos"))]
mod list;

#[cfg(target_os = "linux")]
mod import;

#[cfg(target_os = "linux")]
pub(super) use import::run as import;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use list::run as list;

use std::ffi::OsStr;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serde_saphyr::Tagged;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{DoltSuccess, Snapshot, is_uuid, unverified_authority};
use crate::cli::{CommonOptions, ConversationSelectionArgs, DoltTargetArgs, is_slug};
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

struct Selection {
    candidate: config_v2::Candidate,
    bot_slug: String,
    db_commit: String,
}

impl Selection {
    fn context(&self) -> Value {
        json!({
            "workspace_id": self.candidate.workspace_id(),
            "db_commit": self.db_commit,
            "policy_revision": self.candidate.policy_revision(),
            "scope": "conversation",
        })
    }
}

fn select(
    common: &CommonOptions,
    target: &DoltTargetArgs,
    bot_id: &str,
) -> Result<Selection, HiveError> {
    let expected_id = target
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
    let (mut snapshot, _) = Snapshot::open(target)?;
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
    Ok(Selection {
        candidate,
        bot_slug: bot_slug.clone(),
        db_commit: snapshot.commit,
    })
}

pub(super) fn read(
    common: &CommonOptions,
    args: &ConversationSelectionArgs,
    bot_id: &str,
    conversation_id: &str,
    destination: Option<&Path>,
) -> Result<DoltSuccess, HiveError> {
    let selection = select(common, &args.target, bot_id)?;
    let candidate = &selection.candidate;
    let workspace_id = candidate.workspace_id();
    let bot_slug = &selection.bot_slug;
    let filename = format!("{conversation_id}.md");
    let components = ["conversations", "bots", bot_id, &filename].map(OsStr::new);
    let bytes = candidate
        .root
        .read_bounded(&components, MAX_CONVERSATION_BYTES)?
        .ok_or_else(|| {
            HiveError::new(ErrorCode::NotFound, "The selected conversation is missing.")
        })?;
    let (metadata, body) = validate_file(&bytes, workspace_id, bot_id, bot_slug, conversation_id)?;
    let data = if let Some(destination) = destination {
        publish_export(&candidate.root.path, destination, body.as_bytes())?;
        json!({ "bot_id": bot_id, "conversation_id": conversation_id })
    } else {
        json!({ "metadata": metadata, "body": body })
    };
    Ok(DoltSuccess {
        context: selection.context(),
        data,
    })
}

#[cfg(target_os = "linux")]
fn publish_export(workspace: &Path, destination: &Path, body: &[u8]) -> Result<(), HiveError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    use crate::config_file::ScopedDirectory;

    let invalid_destination = || {
        HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply --destination as a path to a new file.",
        )
    };
    let raw = destination.as_os_str().as_bytes();
    let basename = raw.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
    if matches!(basename, b"" | b"." | b"..") || raw.contains(&0) {
        return Err(invalid_destination());
    }
    let name = CString::new(basename).map_err(|_| invalid_destination())?;
    let parent = destination.parent().ok_or_else(invalid_destination)?;
    let parent = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| {
                HiveError::new(ErrorCode::IoError, "Could not locate the export directory.")
            })?
            .join(parent)
    };
    let directory = ScopedDirectory::open_absolute(&parent).map_err(|error| {
        HiveError::new(
            error.code,
            "Could not open the export directory without following symlinks.",
        )
    })?;
    // Compare the component paths resolved by the no-follow directory walk.
    if directory.path.starts_with(workspace.join("conversations")) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "The export destination must be outside the selected conversation storage.",
        ));
    }
    publish_private(directory.handle(), &name, &[body], || Ok(()))
}

#[cfg(target_os = "linux")]
fn publish_private(
    parent: &std::fs::File,
    name: &std::ffi::CStr,
    chunks: &[&[u8]],
    check_destination: impl FnOnce() -> Result<(), HiveError>,
) -> Result<(), HiveError> {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{Error, ErrorKind, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;

    let parent_fd = parent.as_raw_fd();

    // An unnamed inode belongs to this descriptor until publication. Closing it
    // on any prepublication failure removes it without a pathname cleanup race.
    // SAFETY: parent_fd is open, "." is NUL-terminated, and the creation mode is supplied.
    let fd = unsafe {
        libc::openat(
            parent_fd,
            c".".as_ptr(),
            libc::O_TMPFILE | libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600 as libc::mode_t,
        )
    };
    if fd == -1 {
        return Err(HiveError::new(
            ErrorCode::IoError,
            "Could not create a private temporary file in the conversation destination directory.",
        ));
    }
    // SAFETY: openat returned a newly owned descriptor.
    let mut temporary = unsafe { File::from_raw_fd(fd) };
    let write_error =
        |_| HiveError::new(ErrorCode::IoError, "Could not write the conversation file.");
    temporary
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(write_error)?;
    for chunk in chunks {
        temporary.write_all(chunk).map_err(write_error)?;
    }
    temporary.sync_all().map_err(write_error)?;
    check_destination()?;

    // The source is the kernel's link to our open descriptor. The destination
    // is a single component under the pinned directory and linkat never replaces it.
    let source = CString::new(format!("/proc/self/fd/{}", temporary.as_raw_fd()))
        .expect("a decimal file descriptor cannot contain NUL");
    // SAFETY: Both names are NUL-terminated and the descriptors remain open.
    let result = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            parent_fd,
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };
    if result == -1 {
        return Err(
            if Error::last_os_error().kind() == ErrorKind::AlreadyExists {
                HiveError::new(
                    ErrorCode::AlreadyExists,
                    "The conversation destination already exists.",
                )
            } else {
                HiveError::new(
                    ErrorCode::IoError,
                    "Could not publish the conversation file.",
                )
            },
        );
    }
    parent.sync_all().map_err(|_| {
        HiveError::new(
            ErrorCode::UnknownWriteOutcome,
            "The conversation file was published, but its directory could not be synchronized.",
        )
    })
}

#[cfg(not(target_os = "linux"))]
fn publish_export(_workspace: &Path, _destination: &Path, _body: &[u8]) -> Result<(), HiveError> {
    Err(HiveError::new(
        ErrorCode::NotImplemented,
        "Conversation export is unavailable on this platform.",
    ))
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

    let metadata = validate_metadata(header, workspace_id, bot_id, bot_slug, conversation_id)?;
    if metadata.provenance.body_sha256 != format!("{:x}", Sha256::digest(body.as_bytes())) {
        return Err(invalid("The conversation body digest does not match."));
    }
    Ok((metadata, body))
}

fn validate_metadata(
    header: &str,
    workspace_id: &str,
    bot_id: &str,
    bot_slug: &str,
    conversation_id: &str,
) -> Result<Metadata, HiveError> {
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
    if !matches!(metadata.imported_at.as_bytes().get(10), Some(b'T' | b't'))
        || !OffsetDateTime::parse(&metadata.imported_at, &Rfc3339)
            .is_ok_and(|time| time.offset().is_utc())
        || metadata.imported_at.ends_with("-00:00")
        || metadata
            .title
            .as_deref()
            .is_some_and(|title| !valid_title(title))
        || metadata
            .provenance
            .session_ref
            .as_deref()
            .is_some_and(|reference| !valid_label(reference))
    {
        return Err(invalid("The conversation frontmatter metadata is invalid."));
    }
    Ok(metadata)
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

fn valid_title(title: &str) -> bool {
    !title.is_empty() && valid_label(title) && !title.contains(['\u{2028}', '\u{2029}'])
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

    #[cfg(target_os = "linux")]
    #[test]
    fn conversation_export_publishes_complete_private_bytes_without_clobber() {
        use std::fs;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{PermissionsExt, symlink};
        use std::path::PathBuf;

        struct Sandbox(PathBuf);

        impl Drop for Sandbox {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let mut template = std::env::temp_dir()
            .join("hive-export-XXXXXX")
            .as_os_str()
            .as_bytes()
            .to_vec();
        template.push(0);
        // SAFETY: The template is writable and NUL-terminated.
        assert!(!unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null());
        let sandbox = Sandbox(PathBuf::from(OsStr::from_bytes(
            &template[..template.len() - 1],
        )));
        let root = &sandbox.0;
        let destination = root.join("copy.md");
        let body = "\r\n# 합성\r\n---\nno final newline".as_bytes();

        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| publish_export(root, &destination, body));
            let second = scope.spawn(|| publish_export(root, &destination, body));
            [first.join().unwrap(), second.join().unwrap()]
        });
        let ([Ok(()), Err(error)] | [Err(error), Ok(())]) = results else {
            panic!("Expected exactly one publisher to succeed");
        };
        assert!(matches!(error.code, ErrorCode::AlreadyExists));
        let error = publish_export(root, &destination, b"replacement").unwrap_err();
        assert!(matches!(error.code, ErrorCode::AlreadyExists));
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(root).unwrap().count(), 1);

        let dangling = root.join("dangling.md");
        symlink("missing.md", &dangling).unwrap();
        let error = publish_export(root, &dangling, body).unwrap_err();
        assert!(matches!(error.code, ErrorCode::AlreadyExists));
        assert_eq!(fs::read_link(&dangling).unwrap(), Path::new("missing.md"));
        assert!(!root.join("missing.md").exists());

        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        let redirect = root.join("redirect");
        symlink(&real, &redirect).unwrap();
        for path in [redirect.join("escaped.md"), redirect.join("../escaped.md")] {
            let error = publish_export(root, &path, body).unwrap_err();
            assert!(matches!(error.code, ErrorCode::InvalidConfig));
        }
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        assert!(!root.join("escaped.md").exists());

        for path in [root.join("missing/"), root.join("."), root.join("..")] {
            let error = publish_export(root, &path, body).unwrap_err();
            assert!(matches!(error.code, ErrorCode::InvalidArgument));
        }
        let empty = root.join("empty.md");
        publish_export(root, &empty, b"").unwrap();
        assert!(fs::read(&empty).unwrap().is_empty());
        assert_eq!(fs::read_dir(root).unwrap().count(), 5);

        let storage = root.join("conversations");
        fs::create_dir(&storage).unwrap();
        let blocked = root.join("real/../conversations/copy.md");
        let error = publish_export(root, &blocked, body).unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidArgument));
        assert_eq!(fs::read_dir(&storage).unwrap().count(), 0);
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
        let lowercase = fixture("", "\n").replace("T09:00:00Z", "t09:00:00Z");
        assert!(parse(lowercase.as_bytes()).is_ok());
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
            ("2026-11-27T09:00:00Z", "2026-11-27X09:00:00Z"),
            ("2026-11-27T09:00:00Z", "2026-11-27\\t09:00:00Z"),
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn conversation_list_scans_anchored_files_and_invalidates_changed_pages() {
        use std::fs;
        use std::os::unix::fs::symlink;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        use crate::cli::ConversationListArgs;

        struct TempWorkspace(PathBuf);

        impl Drop for TempWorkspace {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("hive-conversation-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let root = TempWorkspace(path);
        let marker = root.0.join("hive-workspace.yml");
        fs::write(
            &marker,
            format!(
                "version: 2\nworkspace_id: {WORKSPACE}\nschema_version: 1\npolicy_revision: {}\n",
                "a".repeat(40)
            ),
        )
        .unwrap();
        let selection = Selection {
            candidate: config_v2::read_explicit(&root.0.canonicalize().unwrap()).unwrap(),
            bot_slug: SLUG.to_owned(),
            db_commit: "synthetic-test-commit".to_owned(),
        };
        let mut args = ConversationListArgs {
            bot: Some(BOT.into()),
            limit: "1".to_owned(),
            cursor: None,
            target: DoltTargetArgs {
                workspace_id: Some(WORKSPACE.to_owned()),
                database: Some("fixture".to_owned()),
                port: Some("3306".into()),
            },
        };
        let empty = list::page(&selection, &args, BOT, 1).unwrap();
        assert_eq!(empty["items"], json!([]));
        assert_eq!(empty["next_cursor"], Value::Null);

        let directory = root.0.join("conversations/bots").join(BOT);
        fs::create_dir_all(&directory).unwrap();
        let newer = "01900000-0000-7000-8000-000000000004";
        let older_file = directory.join(format!("{CONVERSATION}.md"));
        let newer_file = directory.join(format!("{newer}.md"));
        let mut older_bytes = fixture("", "\r\n").into_bytes();
        older_bytes.extend([0xff, 0]);
        let newer_bytes = fixture("synthetic-body-marker", "\n").replace(CONVERSATION, newer);
        fs::write(&older_file, &older_bytes).unwrap();
        fs::write(&newer_file, &newer_bytes).unwrap();
        let staging = directory.join(".tmp-staging.md");
        symlink("private-fixture-staging-target", &staging).unwrap();
        let first = list::page(&selection, &args, BOT, 1).unwrap();
        assert_eq!(first["items"][0]["conversation_id"], newer);
        assert_eq!(first["truncated"], true);
        assert!(!first.to_string().contains("synthetic-body-marker"));
        args.cursor = Some(first["next_cursor"].as_str().unwrap().to_owned());
        let second = list::page(&selection, &args, BOT, 1).unwrap();
        assert_eq!(second["items"][0]["conversation_id"], CONVERSATION);
        assert_eq!(second["observed_revision"], first["observed_revision"]);
        assert_eq!(second["next_cursor"], Value::Null);
        assert_eq!(fs::read(&older_file).unwrap(), older_bytes);
        assert_eq!(fs::read(&newer_file).unwrap(), newer_bytes.as_bytes());
        assert_eq!(
            fs::read_link(&staging).unwrap(),
            PathBuf::from("private-fixture-staging-target")
        );

        fs::write(
            &newer_file,
            newer_bytes.replace("Synthetic title", "Changed title"),
        )
        .unwrap();
        let error = list::page(&selection, &args, BOT, 1).unwrap_err();
        assert!(matches!(error.code, ErrorCode::StaleCursor));
        args.cursor = None;
        fs::remove_file(&newer_file).unwrap();
        symlink(&older_file, &newer_file).unwrap();
        let error = list::page(&selection, &args, BOT, 1).unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidConfig));
    }
}
