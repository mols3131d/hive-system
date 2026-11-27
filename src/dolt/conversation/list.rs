use std::ffi::{CStr, OsStr};
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::fs::MetadataExt;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Selection, invalid, select, split_frontmatter, validate_metadata};
use crate::cli::{CommonOptions, ConversationListArgs};
use crate::config_file::{MAX_FILE_BYTES, ScopedDirectory, open_regular_child};
use crate::dolt::{DoltFailure, DoltSuccess, is_uuid_v7};
use crate::error::{ErrorCode, HiveError};

const MAX_ENTRIES: usize = 4096;
const MAX_SCAN_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = MAX_FILE_BYTES + 10;

pub(in crate::dolt) fn run(
    common: &CommonOptions,
    args: &ConversationListArgs,
    bot: &str,
    limit: usize,
) -> Result<DoltSuccess, Box<DoltFailure>> {
    let selection = select(common, &args.target, bot).map_err(DoltFailure::without_context)?;
    let context = selection.context();
    match page(&selection, args, bot, limit) {
        Ok(data) => Ok(DoltSuccess { context, data }),
        Err(error) => Err(DoltFailure::with_context(context, error)),
    }
}

pub(super) fn page(
    selection: &Selection,
    args: &ConversationListArgs,
    bot: &str,
    limit: usize,
) -> Result<Value, HiveError> {
    let root = &selection.candidate.root;
    let root_identity = root.file_identity()?;
    let workspace = selection.candidate.workspace_id();
    let scope = format!(
        "{:x}",
        Sha256::digest(
            json!([
                "conversation-list-v1",
                workspace,
                root_identity,
                selection.db_commit,
                &args.target.database,
                args.target
                    .port
                    .as_deref()
                    .and_then(OsStr::to_str)
                    .and_then(|port| port.parse::<u16>().ok()),
                bot,
                selection.bot_slug,
                "conversation_id_desc",
                limit,
            ])
            .to_string()
        )
    );
    let continuation = args
        .cursor
        .as_deref()
        .map(|value| decode(value, &scope))
        .transpose()?;
    let mut items = Vec::new();
    let mut set = Sha256::new();

    let components = ["conversations", "bots", bot].map(OsStr::new);
    let directory = root.open_directory(&components)?;
    let original = directory.as_ref().map(directory_version).transpose()?;
    if let Some(directory) = &directory {
        let ids = published_ids(directory)?;
        let mut remaining = MAX_SCAN_BYTES;
        for id in ids {
            let filename = format!("{id}.md");
            let file = open_regular_child(directory, OsStr::new(&filename))?.ok_or_else(changed)?;
            let bytes = read_frontmatter(file, &mut remaining)?;
            let text = str::from_utf8(&bytes)
                .ok()
                .filter(|text| !text.contains('\0'))
                .ok_or_else(|| {
                    invalid("The conversation frontmatter must be UTF-8 without NUL bytes.")
                })?;
            let (header, _) = split_frontmatter(text)?;
            let metadata = validate_metadata(header, workspace, bot, &selection.bot_slug, &id)?;
            if !is_lower_hex(&metadata.provenance.body_sha256) {
                return Err(invalid("The conversation provenance digest is invalid."));
            }
            set.update(Sha256::digest(&bytes));
            items.push(metadata);
        }
    }
    verify_scan(root, root_identity, &components, original)?;

    let revision = format!("{:x}", set.finalize());
    let start = match continuation {
        Some((offset, digest))
            if offset < items.len() && digest == cursor_digest(offset, &scope, &revision) =>
        {
            offset
        }
        Some(_) => return Err(stale()),
        None => 0,
    };
    let end = start + limit;
    let truncated = end < items.len();
    Ok(json!({
        "items": items.into_iter().skip(start).take(limit).collect::<Vec<_>>(),
        "next_cursor": truncated.then(|| format!("{end}.{scope}.{}", cursor_digest(end, &scope, &revision))),
        "truncated": truncated,
        "observed_revision": revision,
    }))
}

fn cursor_digest(offset: usize, scope: &str, revision: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(json!([offset, scope, revision]).to_string())
    )
}

fn decode<'a>(cursor: &'a str, scope: &str) -> Result<(usize, &'a str), HiveError> {
    if cursor.len() <= 134
        && let Some((offset, tail)) = cursor.split_once('.')
        && let Some((bound, digest)) = tail.split_once('.')
        && bound == scope
        && is_lower_hex(digest)
        && let Ok(index) = offset.parse::<usize>()
        && (1..MAX_ENTRIES).contains(&index)
        && offset == index.to_string()
    {
        return Ok((index, digest));
    }
    Err(stale())
}

fn read_frontmatter(mut input: impl Read, remaining: &mut usize) -> Result<Vec<u8>, HiveError> {
    let mut bytes = Vec::new();
    let mut line_start = 0;
    let mut byte = [0];
    loop {
        if bytes.len() == MAX_HEADER_BYTES {
            return Err(invalid(
                "The conversation frontmatter exceeds the 64 KiB size limit.",
            ));
        }
        if *remaining == 0 {
            return Err(scan_limit());
        }
        // The closing delimiter may be followed immediately by body bytes.
        // A buffered read here would consume some of those bytes.
        input.read_exact(&mut byte).map_err(|error| {
            if error.kind() == ErrorKind::UnexpectedEof {
                invalid("The conversation frontmatter delimiters are invalid.")
            } else {
                read_error()
            }
        })?;
        *remaining -= 1;
        bytes.push(byte[0]);
        if line_start == 0 && !b"---\n".starts_with(&bytes) && !b"---\r\n".starts_with(&bytes) {
            return Err(invalid(
                "The conversation frontmatter delimiters are invalid.",
            ));
        }
        if byte[0] == b'\n' {
            let delimiter = matches!(&bytes[line_start..], b"---\n" | b"---\r\n");
            if line_start != 0 && delimiter {
                return Ok(bytes);
            }
            line_start = bytes.len();
        }
    }
}

pub(super) fn published_ids(directory: &File) -> Result<Vec<String>, HiveError> {
    let mut stream = DirectoryStream::open(directory)?;
    let mut ids = Vec::new();
    let mut scanned = 0;
    while let Some(name) = stream.next()? {
        if matches!(name, b"." | b"..") {
            continue;
        }
        scanned += 1;
        if scanned > MAX_ENTRIES {
            return Err(scan_limit());
        }
        if name.starts_with(b".tmp-") {
            continue;
        }
        let Some(stem) = name.strip_suffix(b".md") else {
            continue;
        };
        let id = str::from_utf8(stem)
            .ok()
            .filter(|id| is_uuid_v7(id))
            .ok_or_else(|| invalid("A published conversation filename is invalid."))?;
        ids.push(id.to_owned());
    }
    ids.sort_unstable_by(|a, b| b.cmp(a));
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(changed());
    }
    Ok(ids)
}

#[derive(Debug, PartialEq, Eq)]
struct DirectoryVersion {
    device: u64,
    inode: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn directory_version(directory: &File) -> Result<DirectoryVersion, HiveError> {
    let info = directory.metadata().map_err(|_| read_error())?;
    Ok(DirectoryVersion {
        device: info.dev(),
        inode: info.ino(),
        modified: (info.mtime(), info.mtime_nsec()),
        changed: (info.ctime(), info.ctime_nsec()),
    })
}

fn verify_scan(
    root: &ScopedDirectory,
    root_identity: (u64, u64),
    components: &[&OsStr],
    original: Option<DirectoryVersion>,
) -> Result<(), HiveError> {
    // A parent can move while the original Queen descriptor and timestamps
    // remain valid. Resolve the same selection again from the anchored root.
    let current = root
        .open_directory(components)
        .map_err(reopen_error)?
        .as_ref()
        .map(directory_version)
        .transpose()?;
    if current != original
        || ScopedDirectory::open_absolute(&root.path)
            .map_err(reopen_error)?
            .file_identity()?
            != root_identity
    {
        return Err(changed());
    }
    Ok(())
}

fn reopen_error(error: HiveError) -> HiveError {
    match error.code {
        ErrorCode::NotFound | ErrorCode::InvalidConfig => changed(),
        _ => error,
    }
}

struct DirectoryStream(*mut libc::DIR);

impl DirectoryStream {
    fn open(directory: &File) -> Result<Self, HiveError> {
        let duplicate = directory.try_clone().map_err(|_| read_error())?;
        // SAFETY: fdopendir receives a live directory descriptor.
        let stream = unsafe { libc::fdopendir(duplicate.as_raw_fd()) };
        if stream.is_null() {
            return Err(read_error());
        }
        // fdopendir owns the descriptor after success.
        let _ = duplicate.into_raw_fd();
        Ok(Self(stream))
    }

    fn next(&mut self) -> Result<Option<&[u8]>, HiveError> {
        #[cfg(target_os = "linux")]
        use libc::__errno_location as errno;
        #[cfg(target_os = "macos")]
        use libc::__error as errno;

        // SAFETY: The stream owns its descriptor. errno is local to this thread.
        unsafe {
            let errno = errno();
            *errno = 0;
            let entry = libc::readdir(self.0);
            if entry.is_null() {
                return if *errno == 0 {
                    Ok(None)
                } else {
                    Err(read_error())
                };
            }
            let name = std::ptr::addr_of!((*entry).d_name).cast();
            Ok(Some(CStr::from_ptr(name).to_bytes()))
        }
    }
}

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: This is the sole owner and closes the stream once.
        unsafe { libc::closedir(self.0) };
    }
}

fn is_lower_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn read_error() -> HiveError {
    HiveError::new(ErrorCode::IoError, "Could not scan conversation metadata.")
}

fn scan_limit() -> HiveError {
    HiveError::new(
        ErrorCode::ContextLimit,
        "The Queen's conversation metadata scan exceeds its limit.",
    )
}

fn stale() -> HiveError {
    HiveError::new(
        ErrorCode::StaleCursor,
        "The conversation cursor is invalid for the selected workspace, Queen, query, or published metadata.",
    )
}

fn changed() -> HiveError {
    HiveError::new(
        ErrorCode::ConcurrentChange,
        "The Queen's conversation directory changed during the scan.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_completion_rejects_replaced_parents_without_leaking_paths() {
        use std::fs;
        use std::os::unix::fs::symlink;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct TempTree(PathBuf);

        impl Drop for TempTree {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("hive-scan-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let fixture = TempTree(path);
        let bot = "01900000-0000-7000-8000-000000000002";
        let conversation = "01900000-0000-7000-8000-000000000003";
        let components = ["conversations", "bots", bot].map(OsStr::new);
        let assert_redacted_change = |error: HiveError| {
            assert_eq!(error.code.exit_status(), 5);
            assert_eq!(
                serde_json::to_value(error).unwrap(),
                json!({
                    "code": "CONCURRENT_CHANGE",
                    "message": "The Queen's conversation directory changed during the scan.",
                })
            );
        };
        for parent in 0..=2 {
            let case = fixture.0.join(parent.to_string());
            let workspace = case.join("private-workspace-fixture");
            let queen = workspace.join("conversations/bots").join(bot);
            let root = {
                fs::create_dir_all(&workspace).unwrap();
                ScopedDirectory::open_absolute(&workspace).unwrap()
            };
            let root_identity = root.file_identity().unwrap();
            verify_scan(&root, root_identity, &components, None).unwrap();
            fs::create_dir_all(&queen).unwrap();
            assert_redacted_change(
                verify_scan(&root, root_identity, &components, None).unwrap_err(),
            );
            fs::write(
                queen.join(format!("{conversation}.md")),
                b"private-fixture-content",
            )
            .unwrap();
            let directory = root.open_directory(&components).unwrap().unwrap();
            let original = directory_version(&directory).unwrap();
            assert_eq!(published_ids(&directory).unwrap(), [conversation]);

            let replaced = queen.ancestors().nth(parent + 1).unwrap();
            let displaced = case.join("private-displaced-fixture");
            fs::rename(replaced, &displaced).unwrap();
            fs::create_dir_all(&queen).unwrap();
            assert_eq!(directory_version(&directory).unwrap(), original);
            assert_redacted_change(
                verify_scan(&root, root_identity, &components, Some(original)).unwrap_err(),
            );

            fs::remove_dir_all(replaced).unwrap();
            symlink(&displaced, replaced).unwrap();
            assert_redacted_change(
                verify_scan(
                    &root,
                    root_identity,
                    &components,
                    Some(directory_version(&directory).unwrap()),
                )
                .unwrap_err(),
            );
        }
    }

    #[test]
    fn frontmatter_scan_stops_at_the_closing_delimiter() {
        let mut budget = MAX_SCAN_BYTES;
        for header in [
            &b"---\nversion: 1\n---\n"[..],
            &b"---\r\nversion: 1\r\n---\r\n"[..],
        ] {
            let bytes = [header, &[0xff, 0]].concat();
            let mut reader = bytes.as_slice();
            assert_eq!(read_frontmatter(&mut reader, &mut budget).unwrap(), header);
            assert_eq!(reader, &[0xff, 0]);
            let mut small_budget = 2;
            let err = read_frontmatter(bytes.as_slice(), &mut small_budget).unwrap_err();
            assert!(matches!(err.code, ErrorCode::ContextLimit));
        }
    }

    #[test]
    fn frontmatter_scan_rejects_missing_and_oversized_headers() {
        let mut budget = MAX_SCAN_BYTES;
        for input in [
            b"---\nversion: 1".to_vec(),
            [&b"---\n"[..], &vec![b'x'; MAX_HEADER_BYTES]].concat(),
        ] {
            let err = read_frontmatter(input.as_slice(), &mut budget).unwrap_err();
            assert!(matches!(err.code, ErrorCode::InvalidRecord));
        }
    }
}
