use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use rustix::fs::{Mode, OFlags, mkdirat, openat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::import::{outside_git, require_private_directory};
use super::list::published_ids;
use super::{MAX_CONVERSATION_BYTES, validate_file};
use crate::config_file::ScopedDirectory;
use crate::dolt::is_uuid_v7;
use crate::error::{ErrorCode, HiveError};

pub(crate) const FORMAT_VERSION: u32 = 2;
const MAX_SNAPSHOTS: usize = 2048;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Conversations {
    files: Vec<Snapshot>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    workspace_id: String,
    bot_id: String,
    bot_slug: String,
    conversation_id: String,
    path: String,
    bytes: usize,
    sha256: String,
}

impl Snapshot {
    fn read(&self, queen: &ScopedDirectory) -> Result<Vec<u8>, HiveError> {
        let bytes = read_snapshot(queen, &self.conversation_id)?;
        if bytes.len() != self.bytes || sha256(&bytes) != self.sha256 {
            return Err(invalid_archive());
        }
        self.validate(&bytes)?;
        Ok(bytes)
    }

    fn validate(&self, bytes: &[u8]) -> Result<(), HiveError> {
        validate_file(
            bytes,
            &self.workspace_id,
            &self.bot_id,
            &self.bot_slug,
            &self.conversation_id,
        )?;
        Ok(())
    }
}

pub(crate) fn capture(
    source: &ScopedDirectory,
    destination: &ScopedDirectory,
    workspace_id: &str,
    queens: &BTreeMap<String, String>,
) -> Result<Conversations, HiveError> {
    outside_git(source)?;
    let output = create_directory(destination.handle(), "conversations")?;
    let mut files = Vec::new();
    let Some(bots) = open_bots(source)? else {
        return Ok(Conversations { files });
    };
    let output_bots = create_directory(&output, "bots")?;
    for (bot_id, bot_slug) in queens {
        let Some(queen) = open_directory(&bots, bot_id)? else {
            continue;
        };
        let ids = published_ids(queen.handle())?;
        if ids.is_empty() {
            continue;
        }
        if files.len() + ids.len() > MAX_SNAPSHOTS {
            return Err(HiveError::new(
                ErrorCode::ContextLimit,
                "The backup contains too many conversation snapshots.",
            ));
        }
        let output_queen = create_directory(&output_bots, bot_id)?;
        for id in ids {
            let bytes = read_snapshot(&queen, &id)?;
            let snapshot = Snapshot {
                workspace_id: workspace_id.to_owned(),
                bot_id: bot_id.clone(),
                bot_slug: bot_slug.clone(),
                path: relative_path(bot_id, &id),
                conversation_id: id,
                bytes: bytes.len(),
                sha256: sha256(&bytes),
            };
            snapshot.validate(&bytes)?;
            write_snapshot(&output_queen, &snapshot.conversation_id, &bytes)?;
            files.push(snapshot);
        }
    }
    Ok(Conversations { files })
}

impl Conversations {
    pub(crate) fn restore(
        &self,
        source: &ScopedDirectory,
        destination: &File,
        workspace_id: &str,
        queens: &BTreeMap<String, String>,
    ) -> Result<(), HiveError> {
        if self.files.len() > MAX_SNAPSHOTS {
            return Err(invalid_archive());
        }
        let mut paths = BTreeSet::new();
        let mut selected = BTreeMap::<_, Vec<_>>::new();
        for snapshot in &self.files {
            if snapshot.workspace_id != workspace_id
                || queens.get(&snapshot.bot_id) != Some(&snapshot.bot_slug)
                || !is_uuid_v7(&snapshot.conversation_id)
                || snapshot.path != relative_path(&snapshot.bot_id, &snapshot.conversation_id)
                || !paths.insert(&snapshot.path)
                || snapshot.bytes > MAX_CONVERSATION_BYTES
                || snapshot.sha256.len() != 64
                || !snapshot
                    .sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(invalid_archive());
            }
            selected.entry(&snapshot.bot_id).or_default().push(snapshot);
        }
        if selected.is_empty() {
            return Ok(());
        }

        let bots = open_bots(source)?.ok_or_else(invalid_archive)?;
        let output_bots = create_directory(destination, "bots")?;
        for (id, snapshots) in selected {
            let queen = open_directory(&bots, id)?.ok_or_else(invalid_archive)?;
            let output_queen = create_directory(&output_bots, id)?;
            for snapshot in snapshots {
                let bytes = snapshot.read(&queen)?;
                write_snapshot(&output_queen, &snapshot.conversation_id, &bytes)?;
            }
        }
        Ok(())
    }
}

fn relative_path(bot_id: &str, id: &str) -> String {
    format!("conversations/bots/{bot_id}/{id}.md")
}

fn open_bots(root: &ScopedDirectory) -> Result<Option<ScopedDirectory>, HiveError> {
    match open_directory(root, "conversations")? {
        Some(conversations) => open_directory(&conversations, "bots"),
        None => Ok(None),
    }
}

fn open_directory(
    parent: &ScopedDirectory,
    name: &str,
) -> Result<Option<ScopedDirectory>, HiveError> {
    let directory = parent.open_child_directory(OsStr::new(name))?;
    if let Some(directory) = &directory {
        require_private_directory(directory)?;
    }
    Ok(directory)
}

fn read_snapshot(queen: &ScopedDirectory, id: &str) -> Result<Vec<u8>, HiveError> {
    let name = format!("{id}.md");
    let file = queen
        .open_regular(OsStr::new(&name))?
        .ok_or_else(invalid_archive)?;
    let before = file.metadata().map_err(io_error)?;
    // SAFETY: geteuid takes no pointers and has no preconditions.
    if before.uid() != unsafe { libc::geteuid() }
        || before.mode() & 0o7777 != 0o600
        || before.nlink() != 1
        || before.len() > MAX_CONVERSATION_BYTES as u64
    {
        return Err(invalid_archive());
    }

    let mut bytes = Vec::new();
    (&file)
        .take(before.len() + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let after = file.metadata().map_err(io_error)?;
    if bytes.len() as u64 != before.len()
        || (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec())
    {
        return Err(invalid_archive());
    }
    Ok(bytes)
}

fn create_directory(parent: &File, name: &str) -> Result<File, HiveError> {
    mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)
        .map_err(|error| io_error(error.into()))?;
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| io_error(error.into()))
}

fn write_snapshot(queen: &File, id: &str, bytes: &[u8]) -> Result<(), HiveError> {
    let name = format!("{id}.md");
    let mut file = File::from(
        openat(
            queen,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| io_error(error.into()))?,
    );
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid_archive() -> HiveError {
    HiveError::new(
        ErrorCode::AuthorityUnverified,
        "A conversation snapshot is missing, unsafe, or does not match the archive.",
    )
}

fn io_error(_: io::Error) -> HiveError {
    HiveError::new(
        ErrorCode::IoError,
        "Could not copy a private conversation snapshot.",
    )
}
