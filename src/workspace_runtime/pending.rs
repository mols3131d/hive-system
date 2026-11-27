use super::*;

const NAME: &str = "pending-request.json";
pub(crate) const MAX_BYTES: usize = 4096;

/// The pinned file descriptor and bytes let confirmation verify the exact entry
/// it is about to clear under the workspace-ID lock.
pub(crate) struct PendingEntry {
    pub(super) file: File,
    pub(crate) bytes: Vec<u8>,
}

impl WriterRuntime<'_> {
    pub(crate) fn pending(&self) -> Result<Option<PendingEntry>, HiveError> {
        read_pending(self.state)
    }

    pub(crate) fn publish_pending(&self, bytes: &[u8]) -> Result<PendingEntry, HiveError> {
        self.check()?;
        publish(self.state, bytes)
    }

    pub(crate) fn clear_pending(&self, entry: PendingEntry) -> Result<(), HiveError> {
        self.check()?;
        clear(self.state, entry)
    }
}

pub(super) fn read_pending(dir: &ScopedDirectory) -> Result<Option<PendingEntry>, HiveError> {
    let Some(file) = dir.open_regular(OsStr::new(NAME))? else {
        return Ok(None);
    };
    require_private_file(&file)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_BYTES {
        return Err(unverified("The pending marker exceeds its size limit."));
    }
    Ok(Some(PendingEntry { file, bytes }))
}

fn publish(dir: &ScopedDirectory, bytes: &[u8]) -> Result<PendingEntry, HiveError> {
    if bytes.len() > MAX_BYTES {
        return Err(unverified("The pending marker exceeds its size limit."));
    }
    let mut staged = tempfile::Builder::new()
        .prefix(".pending-request-")
        .tempfile_in(at(dir, ""))
        .map_err(io_error)?;
    require_private_file(staged.as_file())?;
    staged.write_all(bytes).map_err(io_error)?;
    staged.as_file().sync_all().map_err(io_error)?;
    let staged_name = staged
        .path()
        .file_name()
        .expect("temporary file has a name");
    rustix::fs::renameat_with(
        dir.directory_file(),
        staged_name,
        dir.directory_file(),
        NAME,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| io_error(error.into()))?;
    sync(dir)?;
    let entry = read_pending(dir)?.ok_or_else(|| unverified("The pending marker disappeared."))?;
    if !same_file(staged.as_file(), &entry.file)? || entry.bytes != bytes {
        return Err(unverified("The pending marker changed during publication."));
    }
    Ok(entry)
}

pub(super) fn clear(dir: &ScopedDirectory, entry: PendingEntry) -> Result<(), HiveError> {
    let current =
        read_pending(dir)?.ok_or_else(|| unverified("The pending marker disappeared."))?;
    if !same_file(&entry.file, &current.file)? || entry.bytes != current.bytes {
        return Err(unverified(
            "The pending marker changed during confirmation.",
        ));
    }
    fs::remove_file(at(dir, NAME)).map_err(io_error)?;
    sync(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_refuses_clobber_and_clear_requires_the_exact_entry() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = ScopedDirectory::open_absolute(scratch.path()).unwrap();
        let entry = publish(&dir, b"synthetic intent").unwrap();
        assert!(publish(&dir, b"conflicting intent").is_err());
        assert_eq!(
            read_pending(&dir).unwrap().unwrap().bytes,
            b"synthetic intent"
        );

        fs::rename(at(&dir, NAME), at(&dir, "retained")).unwrap();
        fs::write(at(&dir, NAME), b"synthetic intent").unwrap();
        fs::set_permissions(
            at(&dir, NAME),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        assert!(clear(&dir, entry).is_err());
        let replacement = read_pending(&dir).unwrap().unwrap();
        assert_eq!(replacement.bytes, b"synthetic intent");
        clear(&dir, replacement).unwrap();
        assert!(read_pending(&dir).unwrap().is_none());
    }
}
