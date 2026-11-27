use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use rustix::fs::{Mode, mkdirat};
use rustix::io::Errno;

use super::{
    MAX_BODY_BYTES, Metadata, Provenance, Source, publish_private, valid_label, valid_title,
    validate_metadata,
};
use crate::cli::{CommonOptions, ConversationImportArgs, is_slug};
use crate::config_file::ScopedDirectory;
use crate::config_v2;
use crate::dolt::{DoltSuccess, DoltTarget};
use crate::error::{ErrorCode, HiveError};
use crate::workspace_status::with_exclusive_owned_snapshot;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;

pub(in crate::dolt) fn run(
    common: &CommonOptions,
    args: &ConversationImportArgs,
    bot_id: &str,
) -> Result<DoltSuccess, HiveError> {
    let target = DoltTarget::parse(&args.target)?;
    let workspace = common.workspace.as_deref().ok_or_else(|| {
        HiveError::new(
            ErrorCode::InvalidArgument,
            "Supply an explicit --workspace.",
        )
    })?;
    let candidate = config_v2::read_explicit(workspace)?;
    let workspace_id = candidate.workspace_id();
    with_exclusive_owned_snapshot(
        &candidate,
        target,
        |snapshot| {
            let queens: Vec<(String, String, bool)> = snapshot.exec_rows(
                |commit| {
                    format!(
                        "SELECT id, slug, retired_at IS NULL FROM bots AS OF '{commit}' WHERE CAST(id AS BINARY) = ? LIMIT 2"
                    )
                },
                (bot_id,),
            )?;
            let bot_slug = match queens.as_slice() {
                [(id, slug, true)] if id == bot_id && is_slug(slug) => slug,
                [(_, _, false)] | [] => {
                    return Err(HiveError::new(
                        ErrorCode::NotFound,
                        "The Queen is absent from the active committed registry.",
                    ));
                }
                _ => {
                    return Err(HiveError::new(
                        ErrorCode::InvalidRecord,
                        "The committed Queen identity is invalid.",
                    ));
                }
            };
            if args
                .title
                .as_deref()
                .is_some_and(|title| !valid_title(title))
                || args
                    .session_ref
                    .as_deref()
                    .is_some_and(|reference| !valid_label(reference))
            {
                return Err(HiveError::new(
                    ErrorCode::InvalidArgument,
                    "The conversation title or session reference is invalid.",
                ));
            }
            let (source, body) = read_input(args.file.as_deref())?;
            let elapsed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| io_error("The system clock is before the Unix epoch."))?;
            let now = OffsetDateTime::from_unix_timestamp_nanos(elapsed.as_nanos() as i128)
                .map_err(|_| {
                    io_error("The system clock is outside the supported timestamp range.")
                })?;
            let metadata = Metadata {
                version: json!(1),
                workspace_id: workspace_id.to_owned(),
                bot_id: bot_id.to_owned(),
                bot_slug: bot_slug.clone(),
                conversation_id: conversation_id(now)?,
                imported_at: format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                    now.year(),
                    u8::from(now.month()),
                    now.day(),
                    now.hour(),
                    now.minute(),
                    now.second(),
                ),
                title: args.title.clone(),
                provenance: Provenance {
                    source,
                    session_ref: args.session_ref.clone(),
                    body_sha256: format!("{:x}", Sha256::digest(&body)),
                },
            };
            Ok((snapshot.commit.clone(), metadata, body))
        },
        |(commit, metadata, body)| {
            for ancestor in candidate.root.path.ancestors().skip(1) {
                outside_git(&ScopedDirectory::open_absolute(ancestor)?)?;
            }
            publish(&candidate.root, &metadata, &body)?;
            Ok(DoltSuccess {
                context: json!({
                    "workspace_id": workspace_id,
                    "db_commit": commit,
                    "policy_revision": candidate.policy_revision(),
                    "scope": "conversation",
                }),
                data: json!({ "bot_id": bot_id, "conversation_id": metadata.conversation_id }),
            })
        },
    )
}

fn read_input(path: Option<&Path>) -> Result<(Source, Vec<u8>), HiveError> {
    let (source, input): (_, Box<dyn Read>) = if let Some(path) = path {
        let invalid_path = || {
            HiveError::new(
                ErrorCode::InvalidArgument,
                "Supply a regular --file path without traversal or symlinks.",
            )
        };
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(invalid_path());
        }
        let basename = path
            .as_os_str()
            .as_bytes()
            .rsplit(|byte| *byte == b'/')
            .next();
        let name = basename
            .filter(|name| !matches!(*name, b"" | b"." | b".."))
            .map(OsStr::from_bytes)
            .ok_or_else(invalid_path)?;
        let parent = path.parent().ok_or_else(invalid_path)?;
        let absolute;
        let parent = if parent.is_absolute() {
            parent
        } else {
            absolute = std::env::current_dir()
                .map_err(|_| io_error("Could not locate the input directory."))?
                .join(parent);
            &absolute
        };
        let directory = ScopedDirectory::open_absolute(parent)?;
        let file = directory.open_regular(name)?.ok_or_else(|| {
            HiveError::new(ErrorCode::NotFound, "The selected input file is missing.")
        })?;
        (Source::File, Box::new(file))
    } else {
        (Source::Stdin, Box::new(io::stdin().lock()))
    };
    let mut body = Vec::new();
    input
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| io_error("Could not read the conversation input."))?;
    if body.len() > MAX_BODY_BYTES || body.contains(&0) || std::str::from_utf8(&body).is_err() {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "Conversation input must be UTF-8 without NUL bytes and at most 4 MiB.",
        ));
    }
    Ok((source, body))
}

fn conversation_id(now: OffsetDateTime) -> Result<String, HiveError> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes[6..]))
        .map_err(|_| io_error("Could not allocate a conversation ID."))?;
    let millis = (now.unix_timestamp_nanos() / 1_000_000) as u64;
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = format!("{:032x}", u128::from_be_bytes(bytes));
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn publish(root: &ScopedDirectory, metadata: &Metadata, body: &[u8]) -> Result<(), HiveError> {
    // JSON string quoting also gives YAML-safe scalars for user-provided fields.
    let header = serde_json::to_string(metadata)
        .map_err(|_| io_error("Could not encode conversation metadata."))?;
    validate_metadata(
        &header,
        &metadata.workspace_id,
        &metadata.bot_id,
        &metadata.bot_slug,
        &metadata.conversation_id,
    )?;
    let header = format!("---\n{header}\n---\n");
    outside_git(root)?;
    let conversations = private_child(root, "conversations")?;
    let bots = private_child(&conversations, "bots")?;
    let queen = private_child(&bots, &metadata.bot_id)?;
    let name = CString::new(format!("{}.md", metadata.conversation_id))
        .expect("a generated UUID filename contains no NUL");
    publish_private(queen.handle(), &name, &[header.as_bytes(), body], || {
        for directory in [root, &conversations, &bots, &queen] {
            let current = ScopedDirectory::open_absolute(&directory.path)?;
            if !current.same_directory(directory)? {
                return Err(HiveError::new(
                    ErrorCode::ConcurrentChange,
                    "A conversation directory changed during import.",
                ));
            }
            outside_git(&current)?;
        }
        Ok(())
    }).map_err(|error| {
        if matches!(error.code, ErrorCode::UnknownWriteOutcome) {
            HiveError::new(
                error.code,
                format!(
                    "Conversation {} for Queen {} was published, but its directory could not be synchronized.",
                    metadata.conversation_id, metadata.bot_id,
                ),
            )
        } else {
            error
        }
    })
}

fn outside_git(directory: &ScopedDirectory) -> Result<(), HiveError> {
    if directory.contains(c".git")?
        || (directory.contains(c"HEAD")? && directory.contains(c"objects")?)
    {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Conversation storage must be outside Git repositories.",
        ));
    }
    Ok(())
}

fn private_child(parent: &ScopedDirectory, name: &str) -> Result<ScopedDirectory, HiveError> {
    let name = OsStr::new(name);
    match mkdirat(parent.handle(), name, Mode::RWXU) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => {
            return Err(io_error(
                "Could not create a private conversation directory.",
            ));
        }
    }
    let directory = parent.require_child_directory(name)?;
    let info = directory
        .handle()
        .metadata()
        .map_err(|_| io_error("Could not inspect a conversation directory."))?;
    // SAFETY: geteuid takes no pointers and has no preconditions.
    if info.uid() != unsafe { libc::geteuid() } || info.mode() & 0o7777 != 0o700 {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Conversation directories must be owned by the caller with mode 0700.",
        ));
    }
    outside_git(&directory)?;
    parent
        .handle()
        .sync_all()
        .map_err(|_| io_error("Could not synchronize the conversation directory hierarchy."))?;
    Ok(directory)
}

fn io_error(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::IoError, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dolt::conversation::validate_file;
    use crate::dolt::is_uuid_v7;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn imports_preserve_bytes_and_refuse_unsafe_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = ScopedDirectory::open_absolute(temp.path()).unwrap();
        let input = temp.path().join("input.md");
        let bytes = "\u{feff}---\r\n# 합성\r\n---\nno final newline".as_bytes();
        fs::write(&input, bytes).unwrap();
        let redirected = temp.path().join("redirected.md");
        symlink(&input, &redirected).unwrap();
        assert!(read_input(Some(&redirected)).is_err());
        assert!(read_input(Some(&temp.path().join("../input.md"))).is_err());
        assert!(read_input(Some(temp.path())).is_err());
        for invalid in [&[0xff][..], b"zero\0byte", &vec![b'a'; MAX_BODY_BYTES + 1]] {
            fs::write(&input, invalid).unwrap();
            assert!(read_input(Some(&input)).is_err());
        }
        fs::write(&input, bytes).unwrap();

        let (source, body) = read_input(Some(&input)).unwrap();
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let mut metadata = Metadata {
            version: json!(1),
            workspace_id: "01900000-0000-7000-8000-000000000001".to_owned(),
            bot_id: "01900000-0000-7000-8000-000000000002".to_owned(),
            bot_slug: "queen-fixture".to_owned(),
            conversation_id: conversation_id(now).unwrap(),
            imported_at: "2027-01-15T08:00:00Z".to_owned(),
            title: Some("Fixture: \"quoted\" *title*".to_owned()),
            provenance: Provenance {
                source,
                session_ref: Some("---".to_owned()),
                body_sha256: format!("{:x}", Sha256::digest(&body)),
            },
        };
        let storage = temp.path().join("conversations");
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, &storage).unwrap();
        assert!(publish(&root, &metadata, &body).is_err());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        fs::remove_file(&storage).unwrap();

        let marker = temp.path().join(".git");
        fs::write(&marker, "gitdir: synthetic\n").unwrap();
        assert!(publish(&root, &metadata, &body).is_err());
        assert!(!storage.exists());
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(publish(&root, &metadata, &body).is_err());
        assert!(!storage.exists());
        fs::remove_dir(marker).unwrap();

        let queen = storage.join("bots").join(&metadata.bot_id);
        publish(&root, &metadata, &body).unwrap();
        let first_id = metadata.conversation_id.clone();
        let first = queen.join(format!("{first_id}.md"));
        let original = fs::read(&first).unwrap();
        let error = publish(&root, &metadata, &body).unwrap_err();
        assert!(matches!(error.code, ErrorCode::AlreadyExists));

        metadata.conversation_id = conversation_id(now).unwrap();
        assert_ne!(metadata.conversation_id, first_id);
        publish(&root, &metadata, &body).unwrap();
        assert_eq!(fs::read(&first).unwrap(), original);
        assert_eq!(fs::read_dir(&queen).unwrap().count(), 2);
        assert_eq!(fs::read(&input).unwrap(), bytes);
        for directory in [&storage, &storage.join("bots"), &queen] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for id in [&first_id, &metadata.conversation_id] {
            assert!(is_uuid_v7(id));
            let file = queen.join(format!("{id}.md"));
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let stored = fs::read(file).unwrap();
            let (parsed, markdown) = validate_file(
                &stored,
                &metadata.workspace_id,
                &metadata.bot_id,
                &metadata.bot_slug,
                id,
            )
            .unwrap();
            assert!(matches!(parsed.provenance.source, Source::File));
            assert_eq!(parsed.title, metadata.title);
            assert_eq!(
                parsed.provenance.session_ref,
                metadata.provenance.session_ref
            );
            assert_eq!(markdown.as_bytes(), bytes);
        }
    }
}
