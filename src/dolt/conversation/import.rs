use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use rustix::fs::{AtFlags, Mode, mkdirat, statat};
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
    publish_with(root, metadata, body, || {})
}

fn publish_with(
    root: &ScopedDirectory,
    metadata: &Metadata,
    body: &[u8],
    before_link: impl FnOnce(),
) -> Result<(), HiveError> {
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
    let ancestors = [root, &conversations, &bots];
    let published = publish_private(queen.handle(), &name, &[header.as_bytes(), body], || {
        check_destination(&ancestors, &queen)?;
        before_link();
        Ok(())
    })
    .map_err(|error| {
        if matches!(error.code, ErrorCode::UnknownWriteOutcome) {
            uncertain(metadata, "its directory could not be synchronized")
        } else {
            error
        }
    })?;

    let check_publication = || -> Result<bool, HiveError> {
        let current = check_destination(&ancestors, &queen)?;
        let entry = statat(current.handle(), name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| io_error("Could not inspect the published conversation entry."))?;
        let file = published
            .metadata()
            .map_err(|_| io_error("Could not identify the published conversation inode."))?;
        Ok((entry.st_dev, entry.st_ino) == (file.dev(), file.ino()))
    };
    if !check_publication().unwrap_or(false) {
        return Err(uncertain(
            metadata,
            "its identity at the canonical destination could not be verified",
        ));
    }
    Ok(())
}

fn check_destination(
    ancestors: &[&ScopedDirectory],
    destination: &ScopedDirectory,
) -> Result<ScopedDirectory, HiveError> {
    for directory in ancestors {
        check_directory(directory)?;
    }
    check_directory(destination)
}

fn check_directory(directory: &ScopedDirectory) -> Result<ScopedDirectory, HiveError> {
    let current = ScopedDirectory::open_absolute(&directory.path)?;
    if !current.same_directory(directory)? {
        return Err(HiveError::new(
            ErrorCode::ConcurrentChange,
            "A conversation directory changed during import.",
        ));
    }
    outside_git(&current)?;
    Ok(current)
}

fn uncertain(metadata: &Metadata, reason: &'static str) -> HiveError {
    HiveError::new(
        ErrorCode::UnknownWriteOutcome,
        format!(
            "Conversation {} for Queen {} was published, but {reason}.",
            metadata.conversation_id, metadata.bot_id,
        ),
    )
}

pub(super) fn outside_git(directory: &ScopedDirectory) -> Result<(), HiveError> {
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
    private_child_with(parent, name, || {})
}

fn private_child_with(
    parent: &ScopedDirectory,
    name: &str,
    after_create: impl FnOnce(),
) -> Result<ScopedDirectory, HiveError> {
    let name = OsStr::new(name);
    match create_private_directory(parent.handle(), name) {
        Ok(()) => after_create(),
        Err(Errno::EXIST) => {}
        Err(_) => {
            return Err(io_error(
                "Could not create a private conversation directory.",
            ));
        }
    }
    let directory = parent.require_child_directory(name)?;
    require_private_directory(&directory)?;
    parent
        .handle()
        .sync_all()
        .map_err(|_| io_error("Could not synchronize the conversation directory hierarchy."))?;
    Ok(directory)
}

pub(super) fn require_private_directory(directory: &ScopedDirectory) -> Result<(), HiveError> {
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
    outside_git(directory)
}

fn create_private_directory(parent: &File, name: &OsStr) -> rustix::io::Result<()> {
    struct RestoreUmask(libc::mode_t);
    impl Drop for RestoreUmask {
        fn drop(&mut self) {
            // SAFETY: This restores the mask saved in the same process.
            unsafe { libc::umask(self.0) };
        }
    }

    // Set the mode at creation. A later open can reach a replacement even
    // with no-follow traversal. Only the import CLI (or an isolated test
    // subprocess) changes its mask, and only for this mkdirat.
    // SAFETY: umask takes no pointers and has no preconditions.
    let _restore = RestoreUmask(unsafe { libc::umask(0o077) });
    mkdirat(parent, name, Mode::RWXU)
}

fn io_error(message: &'static str) -> HiveError {
    HiveError::new(ErrorCode::IoError, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dolt::conversation::validate_file;
    use crate::dolt::is_uuid_v7;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    fn fixture_metadata(source: Source, body: &[u8]) -> Metadata {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        Metadata {
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
                body_sha256: format!("{:x}", Sha256::digest(body)),
            },
        }
    }

    fn isolated_fixture(test: &str, umask: libc::mode_t) -> Option<PathBuf> {
        const FIXTURE: &str = "HIVE_IMPORT_UMASK_FIXTURE";
        if let Some(path) = std::env::var_os(FIXTURE) {
            return Some(PathBuf::from(path));
        }

        let temp = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.env(FIXTURE, temp.path()).args([
            "--exact",
            &format!("dolt::conversation::import::tests::{test}"),
        ]);
        // SAFETY: umask is async-signal-safe and changes only the child process.
        unsafe {
            child.pre_exec(move || {
                libc::umask(umask);
                Ok(())
            });
        }
        let output = child.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
        None
    }

    #[test]
    fn substituted_directory_keeps_its_mode_and_data() {
        let Some(temp) = isolated_fixture("substituted_directory_keeps_its_mode_and_data", 0o077)
        else {
            return;
        };
        let path = temp.as_path();
        let root = ScopedDirectory::open_absolute(path).unwrap();
        let expected = path.join("conversations");
        let created = path.join("created");
        let unrelated = path.join("unrelated");
        fs::create_dir(&unrelated).unwrap();
        fs::set_permissions(&unrelated, Permissions::from_mode(0o755)).unwrap();
        let sentinel = b"unrelated synthetic data";
        fs::write(unrelated.join("keep"), sentinel).unwrap();
        let original = fs::metadata(&unrelated).unwrap();

        let result = private_child_with(&root, "conversations", || {
            assert_eq!(fs::metadata(&expected).unwrap().mode() & 0o7777, 0o700);
            fs::rename(&expected, &created).unwrap();
            fs::rename(&unrelated, &expected).unwrap();
        });

        let Err(error) = result else {
            panic!("A substituted directory with mode 0755 was accepted.");
        };
        assert!(matches!(error.code, ErrorCode::InvalidConfig));
        let replacement = fs::metadata(&expected).unwrap();
        assert_eq!(
            (replacement.dev(), replacement.ino()),
            (original.dev(), original.ino())
        );
        assert_eq!(replacement.mode() & 0o7777, 0o755);
        assert_eq!(fs::read(expected.join("keep")).unwrap(), sentinel);
        assert_eq!(fs::read_dir(&expected).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&created).unwrap().count(), 0);
    }

    #[test]
    fn import_reports_uncertainty_when_the_parent_moves_at_link() {
        let Some(temp) = isolated_fixture(
            "import_reports_uncertainty_when_the_parent_moves_at_link",
            0o077,
        ) else {
            return;
        };
        let path = temp.as_path();
        let root = ScopedDirectory::open_absolute(path).unwrap();
        let body = b"synthetic conversation";
        let metadata = fixture_metadata(Source::Stdin, body);
        let canonical = path.join("conversations/bots").join(&metadata.bot_id);
        let relocated = path.join("relocated");
        let error = publish_with(&root, &metadata, body, || {
            fs::rename(&canonical, &relocated).unwrap();
            fs::create_dir(&canonical).unwrap();
        })
        .unwrap_err();

        assert!(matches!(error.code, ErrorCode::UnknownWriteOutcome));
        let diagnostic = error.to_string();
        assert!(diagnostic.contains(&metadata.conversation_id));
        assert!(!diagnostic.contains(path.to_str().unwrap()));
        let name = format!("{}.md", metadata.conversation_id);
        assert!(!canonical.join(&name).exists());
        assert!(fs::read(relocated.join(&name)).unwrap().ends_with(body));
    }

    #[test]
    fn first_import_sets_private_modes_under_restrictive_umask() {
        let Some(temp) = isolated_fixture(
            "first_import_sets_private_modes_under_restrictive_umask",
            0o777,
        ) else {
            return;
        };
        let path = temp.as_path();
        let root = ScopedDirectory::open_absolute(path).unwrap();
        let body = b"synthetic conversation";
        let metadata = fixture_metadata(Source::Stdin, body);
        publish(&root, &metadata, body).unwrap();

        let storage = root.path.join("conversations");
        let bots = storage.join("bots");
        let queen = bots.join(&metadata.bot_id);
        for directory in [&storage, &bots, &queen] {
            assert_eq!(fs::metadata(directory).unwrap().mode() & 0o7777, 0o700);
        }
        let file = queen.join(format!("{}.md", metadata.conversation_id));
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o7777, 0o600);
        assert!(fs::read(file).unwrap().ends_with(body));

        fs::set_permissions(&bots, Permissions::from_mode(0o755)).unwrap();
        let replacement = fixture_metadata(Source::Stdin, body);
        let error = publish(&root, &replacement, body).unwrap_err();
        assert!(matches!(error.code, ErrorCode::InvalidConfig));
        assert_eq!(fs::metadata(&bots).unwrap().mode() & 0o7777, 0o755);
        assert_eq!(fs::read_dir(&queen).unwrap().count(), 1);

        let probe = path.join("restored-mask");
        File::create(&probe).unwrap();
        assert_eq!(fs::metadata(probe).unwrap().mode() & 0o7777, 0);
    }

    #[test]
    fn imports_preserve_bytes_and_refuse_unsafe_paths() {
        let Some(temp) = isolated_fixture("imports_preserve_bytes_and_refuse_unsafe_paths", 0o077)
        else {
            return;
        };
        let path = temp.as_path();
        let root = ScopedDirectory::open_absolute(path).unwrap();
        let input = path.join("input.md");
        let bytes = "\u{feff}---\r\n# 합성\r\n---\nno final newline".as_bytes();
        fs::write(&input, bytes).unwrap();
        let redirected = path.join("redirected.md");
        symlink(&input, &redirected).unwrap();
        assert!(read_input(Some(&redirected)).is_err());
        assert!(read_input(Some(&path.join("../input.md"))).is_err());
        assert!(read_input(Some(path)).is_err());
        for invalid in [&[0xff][..], b"zero\0byte", &vec![b'a'; MAX_BODY_BYTES + 1]] {
            fs::write(&input, invalid).unwrap();
            assert!(read_input(Some(&input)).is_err());
        }
        fs::write(&input, bytes).unwrap();

        let (source, body) = read_input(Some(&input)).unwrap();
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let mut metadata = fixture_metadata(source, &body);
        let storage = path.join("conversations");
        let elsewhere = path.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, &storage).unwrap();
        assert!(publish(&root, &metadata, &body).is_err());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        fs::remove_file(&storage).unwrap();

        let marker = path.join(".git");
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
