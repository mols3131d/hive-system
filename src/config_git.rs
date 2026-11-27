use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::error::{ErrorCode, HiveError};

pub(crate) const MAX_BLOB_BYTES: usize = 128 * 1024;

pub(crate) struct GitWorkspace {
    pub(crate) root: PathBuf,
    pub(crate) revision: Option<String>,
}

pub(crate) fn project_root(start: &Path) -> Result<Option<PathBuf>, HiveError> {
    let start = canonical_directory(start)?;
    let result = git(&start, ["rev-parse", "--show-toplevel"])?;
    if !result.status.success() {
        let diagnostic = result.stderr.split(|byte| *byte == b'\n').next();
        if diagnostic.is_some_and(|line| {
            line == b"fatal: not a git repository (or any of the parent directories): .git"
                || line
                    .starts_with(b"fatal: not a git repository (or any parent up to mount point ")
                || line == b"fatal: this operation must be run in a work tree"
        }) {
            return Ok(None);
        }
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Git could not verify the project worktree root",
        ));
    }

    let root = git_directory(&result.stdout)?;
    if !start.starts_with(&root) {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Git reported a worktree outside the selected directory's ancestry",
        ));
    }
    // core.worktree can point at an ancestor whose own Git directory is different.
    if start != root && worktree_git_dir(&start)? != worktree_git_dir(&root)? {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The reported worktree root selects a different Git repository",
        ));
    }
    Ok(Some(root))
}

fn worktree_git_dir(path: &Path) -> Result<PathBuf, HiveError> {
    let result = git(path, ["rev-parse", "--absolute-git-dir"])?;
    if !result.status.success() {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Git could not verify the worktree's repository directory",
        ));
    }
    git_directory(&result.stdout)
}

pub(crate) fn verify_workspace(path: &Path) -> Result<GitWorkspace, HiveError> {
    let root = canonical_directory(path)?;
    if project_root(&root)?.as_deref() != Some(&root) {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The workspace path must identify a Git worktree root",
        ));
    }

    let common_dir = git(
        &root,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !common_dir.status.success() {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Git could not verify the workspace's common repository directory",
        ));
    }
    git_directory(&common_dir.stdout)?;

    let revision = resolve_commit(&root, "HEAD")?;
    if revision.is_none() && !is_unborn(&root)? {
        return Err(HiveError::new(
            ErrorCode::ObjectUnavailable,
            "The workspace HEAD commit is unavailable locally",
        ));
    }
    Ok(GitWorkspace { root, revision })
}

pub(crate) fn is_full_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn is_full_lowercase_oid(oid: &str) -> bool {
    is_full_oid(oid) && !oid.bytes().any(|byte| byte.is_ascii_uppercase())
}

fn resolve_commit(root: &Path, revision: &str) -> Result<Option<String>, HiveError> {
    let expression = format!("{revision}^{{commit}}");
    let result = git(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &expression,
        ],
    )?;
    if !result.status.success() {
        return Ok(None);
    }
    let oid = git_line(&result.stdout)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .filter(|oid| is_full_oid(oid))
        .ok_or_else(|| HiveError::new(ErrorCode::IoError, "Git returned an invalid commit ID"))?;
    Ok(Some(oid.to_owned()))
}

fn is_unborn(root: &Path) -> Result<bool, HiveError> {
    let reference = git(root, ["symbolic-ref", "--quiet", "HEAD"])?;
    let Some(name) = git_line(&reference.stdout).and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return Ok(false);
    };
    if !reference.status.success() || !name.starts_with("refs/heads/") {
        return Ok(false);
    }
    let result = git(root, ["show-ref", "--exists", "--", name])?;
    Ok(result.status.code() == Some(2))
}

pub(crate) fn verify_remote(root: &Path, name: &str) -> Result<(), HiveError> {
    let fetch_urls = remote_fetch_urls(root, name)?;
    let [fetch] = fetch_urls.as_slice() else {
        return Err(invalid_remote());
    };
    let push = git(root, ["remote", "get-url", "--push", "--all", "--", name])?;
    if !push.status.success() || git_line(&push.stdout).filter(|url| valid_url(url)) != Some(fetch)
    {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The coordination remote must resolve to one matching fetch and push destination",
        ));
    }
    Ok(())
}

pub(crate) fn project_fetch_urls(root: &Path) -> Result<Vec<String>, HiveError> {
    let configured = git(
        root,
        [
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^remote\..+\.url$",
        ],
    )?;
    if !matches!(configured.status.code(), Some(0 | 1)) {
        return Err(invalid_remote());
    }

    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for key in configured
        .stdout
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let name = std::str::from_utf8(key)
            .ok()
            .and_then(|key| key.strip_prefix("remote."))
            .and_then(|key| key.strip_suffix(".url"))
            .ok_or_else(invalid_remote)?;
        if seen.insert(name) {
            for url in remote_fetch_urls(root, name)? {
                urls.push(String::from_utf8(url).map_err(|_| invalid_remote())?);
            }
        }
    }
    Ok(urls)
}

fn remote_fetch_urls(root: &Path, name: &str) -> Result<Vec<Vec<u8>>, HiveError> {
    if name.is_empty() || name.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(invalid_remote());
    }
    // get-url can treat a configured remote's name as a URL when its url is missing.
    let key = format!("remote.{name}.url");
    let configured = git(root, ["config", "--null", "--get-all", &key])?;
    if configured.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !configured.status.success() {
        return Err(invalid_remote());
    }
    // An empty url resets the values Git read from earlier configuration sources.
    let expected = configured
        .stdout
        .strip_suffix(b"\0")
        .ok_or_else(invalid_remote)?
        .rsplit(|byte| *byte == 0)
        .take_while(|url| !url.is_empty())
        .count();
    if expected == 0 {
        return Ok(Vec::new());
    }
    let fetch = git(root, ["remote", "get-url", "--all", "--", name])?;
    if !fetch.status.success() {
        return Err(invalid_remote());
    }
    let urls: Vec<_> = git_line(&fetch.stdout)
        .ok_or_else(invalid_remote)?
        .split(|byte| *byte == b'\n')
        .collect();
    // get-url separates values with newlines, which can also appear inside a URL.
    if urls.len() != expected || !urls.iter().all(|url| valid_url(url)) {
        return Err(invalid_remote());
    }
    Ok(urls.into_iter().map(<[u8]>::to_vec).collect())
}

fn valid_url(url: &[u8]) -> bool {
    !url.iter().any(u8::is_ascii_control) && url.iter().any(|byte| !byte.is_ascii_whitespace())
}

fn invalid_remote() -> HiveError {
    HiveError::new(
        ErrorCode::InvalidConfig,
        "Git could not verify the configured remote fetch destination",
    )
}

pub(crate) fn is_tracked(root: &Path, relative: &Path) -> Result<bool, HiveError> {
    let path = relative_git_path(relative)?;
    let result = execute(
        git_command(root)
            .args(["--literal-pathspecs", "ls-files", "-z", "--"])
            .arg(path),
    )?;
    if !result.status.success() {
        return Err(HiveError::new(
            ErrorCode::IoError,
            "Git could not inspect the configuration path in the index",
        ));
    }
    Ok(result
        .stdout
        .split(|byte| *byte == 0)
        .any(|entry| entry == path.as_encoded_bytes()))
}

pub(crate) fn is_ignored(root: &Path, relative: &Path) -> Result<bool, HiveError> {
    let path = relative_git_path(relative)?;
    // check-ignore rejects literal pathspec mode. Callers use fixed relative paths.
    if path.as_encoded_bytes().starts_with(b":") {
        return Err(invalid_relative_path());
    }
    let result = execute(
        git_command(root)
            .args(["check-ignore", "--quiet", "--"])
            .arg(path),
    )?;
    match result.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(HiveError::new(
            ErrorCode::IoError,
            "Git could not check whether the project configuration path is ignored",
        )),
    }
}

pub(crate) fn read_blob(
    root: &Path,
    revision: &str,
    relative: &Path,
) -> Result<Option<Vec<u8>>, HiveError> {
    let command = || git_command(root);
    let Some(object) = find_blob(root, revision, relative)? else {
        return Ok(None);
    };
    let size = execute(command().args(["cat-file", "-s", &object]))?;
    let size = size
        .status
        .success()
        .then_some(git_line(&size.stdout))
        .flatten()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|size| size.parse::<usize>().ok())
        .ok_or_else(unavailable_blob)?;
    if size > MAX_BLOB_BYTES {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The Git configuration source exceeds 128 KiB",
        ));
    }

    let result = execute(command().args(["cat-file", "blob", &object]))?;
    if !result.status.success() || result.stdout.len() != size {
        return Err(unavailable_blob());
    }
    Ok(Some(result.stdout))
}

#[cfg(target_os = "linux")]
pub(crate) fn read_asset_blob(
    root: &crate::config_file::ScopedDirectory,
    revision: &str,
    relative: &Path,
) -> Result<Option<Vec<u8>>, HiveError> {
    let repository = AssetRepository::open(root)?;
    let body = repository.read(revision, relative)?;
    repository.verify_selection(root)?;
    Ok(body)
}

/// Bind current context to the explicitly selected clean asset HEAD.
#[cfg(target_os = "linux")]
pub(crate) struct PinnedAssets<'a> {
    root: &'a crate::config_file::ScopedDirectory,
    repository: AssetRepository,
    revision: &'a str,
}

#[cfg(target_os = "linux")]
impl<'a> PinnedAssets<'a> {
    pub(crate) fn open(
        root: &'a crate::config_file::ScopedDirectory,
        revision: &'a str,
    ) -> Result<Self, HiveError> {
        if !is_full_lowercase_oid(revision) {
            return Err(HiveError::new(
                ErrorCode::InvalidArgument,
                "Select an exact lowercase asset commit object ID.",
            ));
        }
        let repository = AssetRepository::open(root)?;
        let config = repository
            .git_dir
            .read(OsStr::new("config"))?
            .unwrap_or_default();
        let reader = AssetObjectReader::new(&repository.objects, &config, revision.len())?;
        let git = || reader.command();
        let tree = committed_asset_tree(&git, revision)?;
        verified_asset_object(&git, "tree", &tree, 1024 * 1024)?;
        let pinned = Self {
            root,
            repository,
            revision,
        };
        pinned.recheck()?;
        Ok(pinned)
    }

    pub(crate) fn read_blob(&self, relative: &Path) -> Result<Option<Vec<u8>>, HiveError> {
        self.repository.read(self.revision, relative)
    }

    pub(crate) fn recheck(&self) -> Result<(), HiveError> {
        self.check_status(false)
    }

    fn check_status(&self, include_ignored: bool) -> Result<(), HiveError> {
        self.repository.verify_selection(self.root)?;
        let command = || self.repository.status_command();
        let check_head = || {
            let head = execute_asset(command()?.args(["rev-parse", "--verify", "HEAD"]), 65, None)?;
            if !head.status.success() || git_line(&head.stdout) != Some(self.revision.as_bytes()) {
                return Err(HiveError::new(
                    ErrorCode::StaleRevision,
                    "The asset tree HEAD differs from the selected commit.",
                ));
            }

            Ok(())
        };
        check_head()?;
        let index = execute_asset(
            command()?.args(["ls-files", "-v", "-z"]),
            4 * 1024 * 1024,
            None,
        )?;
        // Lowercase tags mark assume-unchanged entries, and S marks skip-worktree.
        if !index.status.success()
            || index
                .stdout
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
                .any(|entry| !entry.starts_with(b"H "))
        {
            return Err(HiveError::new(
                ErrorCode::AuthorityUnverified,
                "The asset index cannot establish a clean worktree.",
            ));
        }

        let mut status = command()?;
        disable_status_filters(command()?, &mut status)?;
        status.arg("status");
        if include_ignored {
            status.arg("--ignored=matching");
        }
        let result = execute_asset(
            status.args([
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=none",
            ]),
            4 * 1024 * 1024,
            None,
        )?;
        if !result.status.success() {
            return Err(HiveError::new(
                ErrorCode::AuthorityUnverified,
                "Could not verify the selected asset worktree.",
            ));
        }
        if !result.stdout.is_empty() {
            return Err(HiveError::new(
                ErrorCode::DirtyConflict,
                "The selected asset worktree has staged, modified, or untracked entries.",
            ));
        }
        check_head()?;
        self.repository.verify_selection(self.root)
    }
}

#[cfg(target_os = "linux")]
pub(crate) const ASSET_BACKUP_REF: &str = "refs/heads/inspection";

#[cfg(target_os = "linux")]
pub(crate) fn with_asset_bundle<T>(
    root: &crate::config_file::ScopedDirectory,
    scratch: &Path,
    bundle: &Path,
    complete: impl FnOnce(&str, &Path, &dyn Fn() -> Result<(), HiveError>) -> Result<T, HiveError>,
) -> Result<T, HiveError> {
    let repository = AssetRepository::open(root)?;
    let head = execute_asset(
        repository
            .status_command()?
            .args(["rev-parse", "--verify", "HEAD^{commit}"]),
        65,
        None,
    )?;
    let revision = git_line(&head.stdout)
        .and_then(|line| std::str::from_utf8(line).ok())
        .filter(|oid| head.status.success() && is_full_lowercase_oid(oid))
        .ok_or_else(unavailable_blob)?;
    let pinned = PinnedAssets {
        root,
        repository,
        revision,
    };
    let recheck = || pinned.check_status(true);
    recheck()?;

    // Advertise only the selected commit and its ancestry. The isolated
    // repository cannot inherit replace refs, shallow boundaries or remotes.
    let config = pinned
        .repository
        .git_dir
        .read(OsStr::new("config"))?
        .unwrap_or_default();
    let reader =
        AssetObjectReader::new_in(&pinned.repository.objects, &config, revision.len(), scratch)?;
    let head_ref = ASSET_BACKUP_REF;
    let check = |command: &mut Command| {
        let result = execute_asset(command, 64 * 1024, None)?;
        if !result.status.success() {
            return Err(HiveError::new(
                ErrorCode::ObjectUnavailable,
                "Could not create or verify the full assets Git bundle.",
            ));
        }
        Ok(result.stdout)
    };
    check(reader.command()?.args(["update-ref", head_ref, revision]))?;
    check(
        reader
            .command()?
            .args(["bundle", "create"])
            .arg(bundle)
            .arg(head_ref),
    )?;

    let verify = scratch.join("verify-assets.git");
    let object_format = if revision.len() == 64 {
        "sha256"
    } else {
        "sha1"
    };
    check(
        isolated_git_command(scratch)
            .args(["init", "--bare", "--template="])
            .arg(format!("--object-format={object_format}"))
            .arg(&verify),
    )?;
    let command = || isolated_git_command(&verify);
    check(command().args(["bundle", "verify"]).arg(bundle))?;
    let refs = check(command().args(["bundle", "unbundle"]).arg(bundle))?;
    if refs != format!("{revision} {head_ref}\n").as_bytes() {
        return Err(unavailable_blob());
    }
    check(command().args(["update-ref", head_ref, revision]))?;
    check(command().args(["fsck", "--full", "--no-reflogs", revision]))?;
    verify_checkout_tree(command, revision)?;
    // The artifact contains only the verified bundle; Git's local metadata
    // lives in scratch and will be removed with it.
    complete(revision, &verify, &recheck)
}

#[cfg(target_os = "linux")]
pub(crate) fn verify_checkout_tree(
    git: impl Fn() -> Command,
    revision: &str,
) -> Result<HashSet<PathBuf>, HiveError> {
    use std::collections::{HashMap, VecDeque};
    use std::os::unix::ffi::OsStrExt;

    let invalid = || {
        HiveError::new(
            ErrorCode::AuthorityUnverified,
            "The asset tree contains an unsupported entry or unsafe symbolic link.",
        )
    };
    let read = |args: &[&str], limit| {
        let output = execute_asset(git().args(args), limit, None).map_err(|_| invalid())?;
        if !output.status.success() {
            return Err(invalid());
        }
        Ok(output.stdout)
    };
    // These are verified Git objects. Use raw path bytes and resolve every link
    // against the tree before checkout, without traversing host paths.
    let listing = read(&["ls-tree", "-r", "-z", revision], 64 * 1024 * 1024)?;
    let mut files = HashSet::new();
    let mut directories = HashSet::new();
    let mut links = HashMap::new();
    let mut link_blobs = HashMap::new();
    let mut entries = listing.split(|byte| *byte == 0);
    if entries.next_back() != Some(&[][..]) {
        return Err(invalid());
    }
    for entry in entries {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(invalid)?;
        let (metadata, name) = (&entry[..tab], &entry[tab + 1..]);
        if let Some(oid) = metadata.strip_prefix(b"120000 blob ") {
            let oid = std::str::from_utf8(oid)
                .ok()
                .filter(|oid| oid.len() == revision.len() && is_full_lowercase_oid(oid))
                .ok_or_else(invalid)?;
            if let std::collections::hash_map::Entry::Vacant(entry) = link_blobs.entry(oid) {
                let target = read(&["cat-file", "blob", oid], 4095)?;
                if target.is_empty() || target.starts_with(b"/") || target.contains(&0) {
                    return Err(invalid());
                }
                entry.insert(target);
            }
            links.insert(name, oid);
        } else if metadata.starts_with(b"100644 blob ") || metadata.starts_with(b"100755 blob ") {
            files.insert(name);
        } else {
            return Err(invalid());
        }
        // A Git subtree with no checkout entries does not create a directory.
        let mut parent = name;
        while let Some(slash) = parent.iter().rposition(|byte| *byte == b'/') {
            parent = &parent[..slash];
            if !directories.insert(parent) {
                break;
            }
        }
    }

    let mut validated = HashSet::new();
    for name in links.keys() {
        let mut pending: VecDeque<_> = name.split(|byte| *byte == b'/').collect();
        let mut path = Vec::new();
        let mut followed = 0;
        while let Some(part) = pending.pop_front() {
            match part {
                b"" | b"." => continue,
                b".." => {
                    if path.is_empty() {
                        return Err(invalid());
                    }
                    path.truncate(path.iter().rposition(|byte| *byte == b'/').unwrap_or(0));
                }
                _ => {
                    let parent_len = path.len();
                    if parent_len != 0 {
                        path.push(b'/');
                    }
                    path.extend_from_slice(part);
                    if let Some(oid) = links.get(path.as_slice()) {
                        followed += 1;
                        if followed > 40 {
                            return Err(invalid());
                        }
                        path.truncate(parent_len);
                        for part in link_blobs[oid].rsplit(|byte| *byte == b'/') {
                            pending.push_front(part);
                        }
                    } else if !(directories.contains(path.as_slice())
                        || pending.is_empty() && files.contains(path.as_slice()))
                    {
                        return Err(invalid());
                    }
                }
            }
        }
        validated.insert(PathBuf::from(OsStr::from_bytes(name)));
    }
    Ok(validated)
}

#[cfg(target_os = "linux")]
struct AssetRepository {
    assets: crate::config_file::ScopedDirectory,
    git_dir: crate::config_file::ScopedDirectory,
    objects: crate::config_file::ScopedDirectory,
}

#[cfg(target_os = "linux")]
impl AssetRepository {
    fn open(root: &crate::config_file::ScopedDirectory) -> Result<Self, HiveError> {
        let repository = Self::select_directories(root)?;
        repository.verify_layout()?;
        Ok(repository)
    }

    fn select_directories(root: &crate::config_file::ScopedDirectory) -> Result<Self, HiveError> {
        let assets = root.require_child_directory(OsStr::new("assets"))?;
        let git_dir = assets.require_child_directory(OsStr::new(".git"))?;
        let objects = git_dir.require_child_directory(OsStr::new("objects"))?;
        Ok(Self {
            assets,
            git_dir,
            objects,
        })
    }

    fn status_command(&self) -> Result<Command, HiveError> {
        use std::os::unix::ffi::OsStringExt;

        let mut command = isolated_git_command(Path::new("/"));
        // SAFETY: all callers run and reap the child while both directories are open.
        let git_dir = unsafe { self.git_dir.inherit_directory(&mut command)? };
        let assets = unsafe { self.assets.inherit_directory(&mut command)? };
        for (prefix, path) in [
            (b"--git-dir=".as_slice(), git_dir),
            (b"--work-tree=".as_slice(), assets),
        ] {
            let arg = [prefix, path.as_os_str().as_encoded_bytes()].concat();
            command.arg(std::ffi::OsString::from_vec(arg));
        }
        Ok(command)
    }

    fn verify_layout(&self) -> Result<(), HiveError> {
        if self.git_dir.contains(c"commondir")? {
            return Err(HiveError::new(
                ErrorCode::InvalidConfig,
                "The assets directory must have a dedicated Git repository.",
            ));
        }
        self.objects.verify_git_objects()
    }

    fn verify_selection(
        &self,
        root: &crate::config_file::ScopedDirectory,
    ) -> Result<(), HiveError> {
        use crate::config_file::ScopedDirectory;

        let changed = || {
            HiveError::new(
                ErrorCode::ConcurrentChange,
                "The selected assets Git repository or object-store layout changed during inspection.",
            )
        };
        self.verify_layout().map_err(|_| changed())?;
        let current_root = ScopedDirectory::open_absolute(&root.path).map_err(|_| changed())?;
        let current = Self::select_directories(&current_root).map_err(|_| changed())?;
        if !root.same_directory(&current_root)?
            || !self.assets.same_directory(&current.assets)?
            || !self.git_dir.same_directory(&current.git_dir)?
            || !self.objects.same_directory(&current.objects)?
        {
            return Err(changed());
        }
        Ok(())
    }

    fn read(&self, revision: &str, relative: &Path) -> Result<Option<Vec<u8>>, HiveError> {
        validate_asset_blob_selection(revision, relative)?;
        let config = self.git_dir.read(OsStr::new("config"))?.unwrap_or_default();
        let reader = AssetObjectReader::new(&self.objects, &config, revision.len())?;
        read_committed_asset_blob(&|| reader.command(), revision, relative)
    }
}

#[cfg(target_os = "linux")]
struct AssetObjectReader<'a> {
    objects: &'a crate::config_file::ScopedDirectory,
    repository: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl<'a> AssetObjectReader<'a> {
    fn new(
        objects: &'a crate::config_file::ScopedDirectory,
        config: &[u8],
        oid_width: usize,
    ) -> Result<Self, HiveError> {
        Self::new_in(objects, config, oid_width, Path::new("/tmp"))
    }

    fn new_in(
        objects: &'a crate::config_file::ScopedDirectory,
        config: &[u8],
        oid_width: usize,
        scratch: &Path,
    ) -> Result<Self, HiveError> {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let invalid_format = || {
            HiveError::new(
                ErrorCode::InvalidConfig,
                "The assets Git repository has an unsupported object format.",
            )
        };
        let version = asset_config_value(config, "core.repositoryformatversion", Some("int"))?;
        let format = asset_config_value(config, "extensions.objectformat", None)?;
        let format = match (version.as_deref(), format.as_deref()) {
            (None | Some(b"0" | b"1"), None | Some(b"sha1")) => "sha1",
            (Some(b"1"), Some(b"sha256")) => "sha256",
            _ => return Err(invalid_format()),
        };
        if oid_width != if format == "sha256" { 64 } else { 40 } {
            return Err(unavailable_blob());
        }

        // Git's object commands require repository metadata. Only the selected
        // object directory is shared with the source repository.
        let io_error = |_| {
            HiveError::new(
                ErrorCode::IoError,
                "Could not create isolated Git reader metadata.",
            )
        };
        let repository = tempfile::Builder::new()
            .prefix("hive-soul-git-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(scratch)
            .map_err(io_error)?;
        let path = repository.path();
        fs::create_dir(path.join("refs")).map_err(io_error)?;
        fs::write(path.join("HEAD"), b"ref: refs/heads/inspection\n").map_err(io_error)?;
        fs::write(
            path.join("config"),
            format!("[core]\nrepositoryformatversion = 1\nbare = true\n[extensions]\nobjectformat = {format}\n"),
        ).map_err(io_error)?;
        Ok(Self {
            objects,
            repository,
        })
    }

    fn command(&self) -> Result<Command, HiveError> {
        let path = self.repository.path();
        let mut command = isolated_git_command(path);
        command.arg("--git-dir=.");
        // SAFETY: The object directory remains open until all children are reaped.
        let objects = unsafe { self.objects.inherit_directory(&mut command)? };
        command.env("GIT_OBJECT_DIRECTORY", objects);
        Ok(command)
    }
}

#[cfg(target_os = "linux")]
fn validate_asset_blob_selection(revision: &str, relative: &Path) -> Result<(), HiveError> {
    if !is_full_oid(revision) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "A snapshot requires a full Git commit object ID",
        ));
    }
    relative_git_path(relative)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn read_committed_asset_blob(
    git: &impl Fn() -> Result<Command, HiveError>,
    revision: &str,
    relative: &Path,
) -> Result<Option<Vec<u8>>, HiveError> {
    validate_asset_blob_selection(revision, relative)?;
    let mut oid = committed_asset_tree(git, revision)?;
    let mut path = relative.components().peekable();
    while let Some(component) = path.next() {
        let tree = verified_asset_object(git, "tree", &oid, 1024 * 1024)?;
        let Some(child) = asset_tree_child(
            &tree,
            component.as_os_str().as_encoded_bytes(),
            revision.len() / 2,
            path.peek().is_none(),
        )?
        else {
            return Ok(None);
        };
        oid = child;
    }
    verified_asset_object(git, "blob", &oid, MAX_BLOB_BYTES).map(Some)
}

#[cfg(target_os = "linux")]
fn committed_asset_tree(
    git: &impl Fn() -> Result<Command, HiveError>,
    revision: &str,
) -> Result<String, HiveError> {
    let commit = verified_asset_object(git, "commit", &revision.to_ascii_lowercase(), 1024 * 1024)?;
    commit
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| line.strip_prefix(b"tree "))
        .and_then(|oid| std::str::from_utf8(oid).ok())
        .filter(|oid| oid.len() == revision.len() && is_full_oid(oid))
        .map(str::to_ascii_lowercase)
        .ok_or_else(unavailable_blob)
}

#[cfg(target_os = "linux")]
fn verified_asset_object(
    git: &impl Fn() -> Result<Command, HiveError>,
    kind: &str,
    oid: &str,
    limit: usize,
) -> Result<Vec<u8>, HiveError> {
    let result = execute_asset(git()?.args(["cat-file", kind, oid]), limit, None)?;
    if !result.status.success() {
        return Err(unavailable_blob());
    }
    let hash = execute_asset(
        git()?.args(["hash-object", "-t", kind, "--stdin", "--no-filters"]),
        65,
        Some(&result.stdout),
    )?;
    if !hash.status.success() || git_line(&hash.stdout) != Some(oid.as_bytes()) {
        return Err(unavailable_blob());
    }
    Ok(result.stdout)
}

#[cfg(target_os = "linux")]
fn isolated_git_command(root: &Path) -> Command {
    let mut command = git_command(root);
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    command
}

#[cfg(target_os = "linux")]
fn asset_config_value(
    config: &[u8],
    key: &str,
    value_type: Option<&str>,
) -> Result<Option<Vec<u8>>, HiveError> {
    let mut command = isolated_git_command(Path::new("/"));
    command.args(["--git-dir=/dev/null", "config", "--file=-", "--no-includes"]);
    if let Some(value_type) = value_type {
        command.args(["--type", value_type]);
    }
    let output = execute_asset(command.args(["--get", key]), 32, Some(config))?;
    match output.status.code() {
        Some(0) if git_line(&output.stdout).is_some() => {
            Ok(git_line(&output.stdout).map(<[u8]>::to_vec))
        }
        Some(1) if output.stdout.is_empty() => Ok(None),
        _ => Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Could not determine the assets Git repository format.",
        )),
    }
}

#[cfg(target_os = "linux")]
fn asset_tree_child(
    mut tree: &[u8],
    name: &[u8],
    oid_bytes: usize,
    leaf: bool,
) -> Result<Option<String>, HiveError> {
    use std::fmt::Write;

    let mut found = None;
    while !tree.is_empty() {
        // Each raw tree entry is "<mode> <name>\0" plus a binary object ID.
        let nul = tree
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(unavailable_blob)?;
        let mut header = tree[..nul].splitn(2, |byte| *byte == b' ');
        let mode = header.next().ok_or_else(unavailable_blob)?;
        let entry = header.next().ok_or_else(unavailable_blob)?;
        let (oid, rest) = tree[nul + 1..]
            .split_at_checked(oid_bytes)
            .ok_or_else(unavailable_blob)?;
        tree = rest;
        if entry != name {
            continue;
        }
        if found.is_some() {
            return Err(unavailable_blob());
        }
        let matches_path_type = if leaf {
            matches!(mode, b"100644" | b"100755")
        } else {
            mode == b"40000"
        };
        if !matches_path_type {
            return Err(HiveError::new(
                ErrorCode::InvalidConfig,
                "The committed Soul path must use directories and end in a regular file.",
            ));
        }
        let mut hex = String::with_capacity(2 * oid_bytes);
        for byte in oid {
            write!(hex, "{byte:02x}").expect("writing to a string cannot fail");
        }
        found = Some(hex);
    }
    Ok(found)
}

fn find_blob(root: &Path, revision: &str, relative: &Path) -> Result<Option<String>, HiveError> {
    if !is_full_oid(revision) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "A snapshot requires a full Git commit object ID",
        ));
    }
    let Some(commit) = resolve_commit(root, revision)? else {
        return Err(unavailable_blob());
    };
    if !commit.eq_ignore_ascii_case(revision) {
        return Err(unavailable_blob());
    }

    let path = relative_git_path(relative)?;
    let result = execute(
        git_command(root)
            .args([
                "--literal-pathspecs",
                "ls-tree",
                "--full-tree",
                "-z",
                &commit,
                "--",
            ])
            .arg(path),
    )?;
    if !result.status.success() {
        return Err(unavailable_blob());
    }
    if result.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(tree_blob_oid(&result.stdout, path)?.to_owned()))
}

#[cfg(target_os = "linux")]
fn execute_asset(
    command: &mut Command,
    limit: usize,
    input: Option<&[u8]>,
) -> Result<Output, HiveError> {
    use std::io::{Read, Write};

    command.stderr(Stdio::null()).stdout(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let io_error = || {
        HiveError::new(
            ErrorCode::IoError,
            "Could not read the assets Git subprocess",
        )
    };
    let mut child = command.spawn().map_err(|_| io_error())?;
    let mut stdout = Vec::new();
    let collected = (|| {
        if let Some(input) = input {
            // hash-object consumes its input before writing a fixed-size object ID.
            child.stdin.take().expect("piped stdin").write_all(input)?;
        }
        child
            .stdout
            .take()
            .expect("piped stdout")
            .take(limit as u64 + 1)
            .read_to_end(&mut stdout)
    })();
    let overflow = stdout.len() > limit;
    if collected.is_err() || overflow {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|_| io_error())?;
    collected.map_err(|_| io_error())?;
    if overflow {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "Git output for committed Soul inspection exceeds its size limit.",
        ));
    }
    Ok(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

fn tree_blob_oid<'a>(entry: &'a [u8], path: &OsStr) -> Result<&'a str, HiveError> {
    let mut entry = entry.splitn(2, |byte| *byte == b'\t');
    let (Some(metadata), Some(name)) = (entry.next(), entry.next()) else {
        return Err(unavailable_blob());
    };
    if name.strip_suffix(b"\0") != Some(path.as_encoded_bytes()) {
        return Err(unavailable_blob());
    }
    let oid = metadata
        .strip_prefix(b"100644 blob ")
        .or_else(|| metadata.strip_prefix(b"100755 blob "))
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidConfig,
                "The Git configuration source must be a regular file",
            )
        })?;
    std::str::from_utf8(oid)
        .ok()
        .filter(|oid| is_full_oid(oid))
        .ok_or_else(unavailable_blob)
}

fn unavailable_blob() -> HiveError {
    HiveError::new(
        ErrorCode::ObjectUnavailable,
        "A required Git snapshot object is unavailable locally",
    )
}

fn disable_status_filters(mut config: Command, command: &mut Command) -> Result<(), HiveError> {
    let filters = execute(config.args([
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        r"^filter\..*\.(clean|process|required)$",
    ]))?;
    if !matches!(filters.status.code(), Some(0 | 1)) {
        return Err(HiveError::new(
            ErrorCode::IoError,
            "Git could not inspect worktree filter configuration",
        ));
    }
    // Git status can start external clean filters when stat data is stale.
    for key in filters
        .stdout
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = std::str::from_utf8(key).map_err(|_| {
            HiveError::new(
                ErrorCode::IoError,
                "Git returned an invalid worktree filter configuration",
            )
        })?;
        let value = if key.ends_with(".required") {
            "false"
        } else {
            ""
        };
        command.args(["-c", &format!("{key}={value}")]);
    }
    Ok(())
}

pub(crate) fn status(root: &Path) -> Result<bool, HiveError> {
    let mut command = git_command(root);
    disable_status_filters(git_command(root), &mut command)?;
    let result = execute(command.args([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=normal",
        "--ignore-submodules=dirty",
    ]))?;
    if !result.status.success() {
        return Err(HiveError::new(
            ErrorCode::IoError,
            "Git could not inspect workspace status locally",
        ));
    }
    Ok(!result.stdout.is_empty())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, HiveError> {
    let path = path.canonicalize().map_err(|error| {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
                ErrorCode::InvalidConfig
            }
            _ => ErrorCode::IoError,
        };
        HiveError::new(code, "The selected directory could not be resolved")
    })?;
    if !path.is_dir() {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The selected path is not a directory",
        ));
    }
    Ok(path)
}

fn git_directory(stdout: &[u8]) -> Result<PathBuf, HiveError> {
    let path = git_line(stdout)
        .and_then(path_from_bytes)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::InvalidConfig,
                "Git returned an invalid repository directory",
            )
        })?;
    canonical_directory(path)
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> Option<&Path> {
    use std::os::unix::ffi::OsStrExt;
    Some(Path::new(OsStr::from_bytes(bytes)))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> Option<&Path> {
    std::str::from_utf8(bytes).ok().map(Path::new)
}

fn relative_git_path(relative: &Path) -> Result<&OsStr, HiveError> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid_relative_path());
    }
    Ok(relative.as_os_str())
}

fn invalid_relative_path() -> HiveError {
    HiveError::new(
        ErrorCode::InvalidConfig,
        "Git requires a literal path relative to the worktree",
    )
}

fn git_line(bytes: &[u8]) -> Option<&[u8]> {
    bytes.strip_suffix(b"\n").filter(|line| !line.is_empty())
}

fn git<I, S>(root: &Path, args: I) -> Result<Output, HiveError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    execute(git_command(root).args(args))
}

fn git_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "--no-pager",
            "--no-replace-objects",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(root)
        .env("LC_ALL", "C")
        .env("GIT_ALLOW_PROTOCOL", "")
        .stdin(Stdio::null());

    // Keep absolute HOME and XDG paths so Git evaluates native conditional includes.
    // Git can interpret a relative XDG_CONFIG_HOME inside the repository after -C.
    for key in ["HOME", "XDG_CONFIG_HOME"] {
        if env::var_os(key).is_some_and(|path| !Path::new(&path).is_absolute()) {
            command.env_remove(key);
        }
    }
    for key in env::vars_os().map(|(key, _)| key) {
        let name = key.to_string_lossy();
        if name.starts_with("GIT_CONFIG_")
            || name.starts_with("GIT_TRACE")
            || name.starts_with("GIT_TEST_")
            || matches!(
                name.as_ref(),
                "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                    | "GIT_ATTR_SOURCE"
                    | "GIT_CEILING_DIRECTORIES"
                    | "GIT_COMMON_DIR"
                    | "GIT_CONFIG"
                    | "GIT_DIR"
                    | "GIT_DISCOVERY_ACROSS_FILESYSTEM"
                    | "GIT_EXEC_PATH"
                    | "GIT_GLOB_PATHSPECS"
                    | "GIT_GRAFT_FILE"
                    | "GIT_ICASE_PATHSPECS"
                    | "GIT_IMPLICIT_WORK_TREE"
                    | "GIT_INDEX_FILE"
                    | "GIT_LITERAL_PATHSPECS"
                    | "GIT_NOGLOB_PATHSPECS"
                    | "GIT_NO_REPLACE_OBJECTS"
                    | "GIT_OBJECT_DIRECTORY"
                    | "GIT_PREFIX"
                    | "GIT_QUARANTINE_PATH"
                    | "GIT_REDIRECT_STDERR"
                    | "GIT_REDIRECT_STDOUT"
                    | "GIT_REPLACE_REF_BASE"
                    | "GIT_SHALLOW_FILE"
                    | "GIT_WORK_TREE"
            )
        {
            command.env_remove(&key);
        }
    }
    command
        .env("GIT_TRACE2", "0")
        .env("GIT_TRACE2_EVENT", "0")
        .env("GIT_TRACE2_PERF", "0");
    command
}

fn execute(command: &mut Command) -> Result<Output, HiveError> {
    let output = command.output().map_err(|_| {
        HiveError::new(
            ErrorCode::IoError,
            "Could not run native Git for local inspection",
        )
    })?;
    if !output.status.success()
        && output
            .stderr
            .starts_with(b"unknown option: --no-lazy-fetch")
    {
        return Err(HiveError::new(
            ErrorCode::NotImplemented,
            "Local inspection requires native Git with --no-lazy-fetch support",
        ));
    }
    Ok(output)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{fs, os::unix::fs::symlink, time::SystemTime};

    use super::*;
    use crate::config_file::ScopedDirectory;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            Self::with_object_format("sha1")
        }

        fn with_object_format(format: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let fixture = Self(
                env::temp_dir().join(format!("hive-asset-test-{}-{nonce}", std::process::id())),
            );
            fs::create_dir(fixture.assets().parent().unwrap()).unwrap();
            fs::create_dir(fixture.assets()).unwrap();
            let format = format!("--object-format={format}");
            fixture.git(&["init", "--initial-branch=main", &format]);
            fixture
        }

        fn assets(&self) -> PathBuf {
            self.0.join("assets")
        }

        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .env_clear()
                .env("HOME", &self.0)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("PATH", env::var_os("PATH").unwrap())
                .current_dir(self.assets())
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn asset_reads_use_the_declared_format_without_repository_config_includes() {
        for format in ["sha1", "sha256"] {
            let fixture = Fixture::with_object_format(format);
            let assets = fixture.assets();
            let relative = Path::new("bots/queen/SOUL.md");
            let soul = assets.join(relative);
            fs::create_dir_all(soul.parent().unwrap()).unwrap();
            fs::write(&soul, "synthetic").unwrap();
            fixture.git(&["add", "--all"]);
            fixture.git(&["commit", "-m", "Synthetic Soul"]);
            let commit = fixture.git(&["rev-parse", "HEAD"]);
            fixture.git(&["repack", "-ad"]);

            let git_dir = assets.join(".git");
            let config = git_dir.join("config");
            let mut original = fs::read(&config).unwrap();
            original.extend_from_slice(b"\n[include]\npath = broken-include\n");
            fs::write(&config, &original).unwrap();
            fs::write(git_dir.join("broken-include"), b"[invalid\n").unwrap();

            let ordinary = git_command(&assets)
                .args(["cat-file", "commit", &commit])
                .output()
                .unwrap();
            assert!(!ordinary.status.success());
            let directory = ScopedDirectory::open_absolute(&fixture.0).unwrap();
            assert_eq!(
                read_asset_blob(&directory, &commit, relative)
                    .unwrap()
                    .unwrap(),
                b"synthetic",
                "{format}"
            );
            assert_eq!(fs::read(&config).unwrap(), original);
        }
    }

    #[test]
    fn asset_blob_reads_only_a_bounded_regular_entry_at_the_selected_commit() {
        let root = Fixture::new();
        let assets = root.assets();
        let relative = Path::new("bots/queen/SOUL.md");
        let soul = assets.join(relative);
        fs::create_dir_all(soul.parent().unwrap()).unwrap();
        let git = |args: &[&str]| root.git(args);
        let save = || {
            git(&["add", "--all"]);
            git(&["commit", "-m", "Synthetic asset snapshot"]);
            git(&["rev-parse", "HEAD"])
        };
        fs::write(soul.with_file_name("README.md"), "management").unwrap();
        let missing = save();
        fs::write(&soul, "committed").unwrap();
        let committed = save();
        let blob = git(&["rev-parse", "HEAD:bots/queen/SOUL.md"]);
        fs::write(&soul, vec![b'x'; MAX_BLOB_BYTES + 1]).unwrap();
        let oversized = save();
        fs::remove_file(&soul).unwrap();
        symlink("README.md", &soul).unwrap();
        let linked = save();
        fs::remove_file(&soul).unwrap();
        fs::write(&soul, "staged").unwrap();
        git(&["add", "--all"]);
        fs::write(&soul, "dirty").unwrap();
        let index = fs::read(assets.join(".git/index")).unwrap();

        let directory = ScopedDirectory::open_absolute(&root.0).unwrap();
        let read = |revision| read_asset_blob(&directory, revision, relative);
        assert_eq!(read(&committed).unwrap().unwrap(), b"committed");
        assert!(read(&missing).unwrap().is_none());
        let read_snapshot = |revision| {
            read_committed_asset_blob(&|| Ok(isolated_git_command(&assets)), revision, relative)
        };
        assert_eq!(read_snapshot(&committed).unwrap().unwrap(), b"committed");
        assert!(matches!(
            read_snapshot(&linked).unwrap_err().code,
            ErrorCode::InvalidConfig
        ));
        for (revision, expected) in [
            (&linked[..], ErrorCode::InvalidConfig),
            (&oversized, ErrorCode::InvalidConfig),
            (&blob, ErrorCode::ObjectUnavailable),
            (
                "0000000000000000000000000000000000000000",
                ErrorCode::ObjectUnavailable,
            ),
            ("HEAD", ErrorCode::InvalidArgument),
            (&committed[..12], ErrorCode::InvalidArgument),
        ] {
            assert_eq!(
                std::mem::discriminant(&read(revision).unwrap_err().code),
                std::mem::discriminant(&expected)
            );
        }
        assert_eq!(fs::read(assets.join(".git/index")).unwrap(), index);
        assert_eq!(git(&["rev-parse", "HEAD"]), linked);
        assert_eq!(fs::read_to_string(&soul).unwrap(), "dirty");

        fs::rename(&assets, root.0.join("elsewhere")).unwrap();
        symlink("elsewhere", &assets).unwrap();
        assert!(matches!(
            read(&committed).unwrap_err().code,
            ErrorCode::InvalidConfig
        ));
        fs::remove_file(&assets).unwrap();
        fs::create_dir(&assets).unwrap();
        symlink("../elsewhere/.git", assets.join(".git")).unwrap();
        assert!(matches!(
            read(&committed).unwrap_err().code,
            ErrorCode::InvalidConfig
        ));
        fs::remove_file(assets.join(".git")).unwrap();
        assert!(matches!(
            read(&committed).unwrap_err().code,
            ErrorCode::NotFound
        ));
    }

    #[test]
    fn asset_repository_rejects_external_git_object_storage() {
        let fixture = Fixture::new();
        let assets = fixture.assets();
        let relative = Path::new("bots/queen/SOUL.md");
        let soul = assets.join(relative);
        fs::create_dir_all(soul.parent().unwrap()).unwrap();
        fs::write(&soul, "synthetic").unwrap();
        fixture.git(&["add", "--all"]);
        fixture.git(&["commit", "-m", "Synthetic Soul"]);
        let commit = fixture.git(&["rev-parse", "HEAD"]);
        let entry = format!("{commit}:bots/queen/SOUL.md");
        let oid = fixture.git(&["rev-parse", &entry]);
        let objects = assets.join(".git/objects");
        let outside = fixture.0.join("other-object-store");
        let directory = ScopedDirectory::open_absolute(&fixture.0).unwrap();
        let inspect = || read_asset_blob(&directory, &commit, relative);
        let rejects_redirect = || {
            assert_eq!(fixture.git(&["cat-file", "blob", &entry]), "synthetic");
            assert!(matches!(
                inspect().unwrap_err().code,
                ErrorCode::InvalidConfig
            ));
        };

        fs::rename(&objects, &outside).unwrap();
        fs::create_dir_all(objects.join("info")).unwrap();
        let alternates = objects.join("info/alternates");
        fs::write(&alternates, format!("{}\n", outside.display())).unwrap();
        rejects_redirect();
        fs::remove_file(alternates).unwrap();
        fs::remove_dir(objects.join("info")).unwrap();
        fs::remove_dir(&objects).unwrap();

        symlink(&outside, &objects).unwrap();
        rejects_redirect();
        fs::remove_file(&objects).unwrap();
        fs::rename(&outside, &objects).unwrap();

        let loose = objects.join(&oid[..2]).join(&oid[2..]);
        fs::rename(&loose, &outside).unwrap();
        symlink(&outside, &loose).unwrap();
        rejects_redirect();
        fs::remove_file(&loose).unwrap();
        fs::rename(&outside, &loose).unwrap();

        fixture.git(&["repack", "-ad"]);
        assert_eq!(inspect().unwrap().unwrap(), b"synthetic");
        let pack = fs::read_dir(objects.join("pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension() == Some(OsStr::new("pack")))
            .unwrap();
        fs::rename(&pack, &outside).unwrap();
        symlink(&outside, &pack).unwrap();
        rejects_redirect();
    }

    #[test]
    fn asset_reads_stay_pinned_when_the_objects_directory_is_swapped() {
        let fixture = Fixture::new();
        let foreign = Fixture::new();
        let relative = Path::new("SOUL.md");
        let save = |fixture: &Fixture, content| {
            fs::write(fixture.assets().join(relative), content).unwrap();
            fixture.git(&["add", "--all"]);
            fixture.git(&["commit", "-m", "Synthetic Soul"]);
            fixture.git(&["rev-parse", "HEAD"])
        };
        let selected = save(&fixture, "selected");
        let replacement = save(&foreign, "replacement");
        let root = ScopedDirectory::open_absolute(&fixture.0).unwrap();
        let repository = AssetRepository::open(&root).unwrap();

        let objects = fixture.assets().join(".git/objects");
        let original = fixture.0.join("original-objects");
        fs::rename(&objects, &original).unwrap();
        fs::rename(foreign.assets().join(".git/objects"), &objects).unwrap();
        assert_eq!(
            fixture.git(&["cat-file", "blob", &format!("{replacement}:SOUL.md")]),
            "replacement"
        );
        assert_eq!(
            repository.read(&selected, relative).unwrap().unwrap(),
            b"selected"
        );
        assert!(matches!(
            repository.read(&replacement, relative).unwrap_err().code,
            ErrorCode::ObjectUnavailable
        ));
        assert!(matches!(
            repository.verify_selection(&root).unwrap_err().code,
            ErrorCode::ConcurrentChange
        ));

        fs::rename(&objects, foreign.assets().join(".git/objects")).unwrap();
        fs::rename(&original, &objects).unwrap();
        assert!(repository.verify_selection(&root).is_ok());
        fs::write(objects.join("info/alternates"), "\n").unwrap();
        assert!(matches!(
            repository.verify_selection(&root).unwrap_err().code,
            ErrorCode::ConcurrentChange
        ));
    }

    #[test]
    fn asset_path_rejects_a_substituted_intermediate_tree() {
        let fixture = Fixture::new();
        let assets = fixture.assets();
        let relative = Path::new("bots/queen/SOUL.md");
        for (directory, body) in [("bots", "selected"), ("other", "substituted")] {
            let path = assets.join(directory).join("queen/SOUL.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        fixture.git(&["add", "--all"]);
        fixture.git(&["commit", "-m", "Synthetic trees"]);
        let commit = fixture.git(&["rev-parse", "HEAD"]);
        let root = ScopedDirectory::open_absolute(&fixture.0).unwrap();
        let inspect = || read_asset_blob(&root, &commit, relative);
        assert_eq!(inspect().unwrap().unwrap(), b"selected");

        let loose = |entry| {
            let oid = fixture.git(&["rev-parse", entry]);
            assets.join(".git/objects").join(&oid[..2]).join(&oid[2..])
        };
        let tree = loose("HEAD:bots");
        fs::remove_file(&tree).unwrap();
        fs::copy(loose("HEAD:other"), tree).unwrap();
        assert_eq!(
            fixture.git(&["cat-file", "blob", "HEAD:bots/queen/SOUL.md"]),
            "substituted"
        );
        assert!(matches!(
            inspect().unwrap_err().code,
            ErrorCode::ObjectUnavailable
        ));
    }

    #[test]
    fn asset_blob_rejects_bytes_that_do_not_match_the_selected_id() {
        let fixture = Fixture::new();
        let assets = fixture.assets();
        let relative = Path::new("SOUL.md");
        fs::write(assets.join(relative), "original").unwrap();
        fs::write(assets.join("other"), "modified").unwrap();
        fixture.git(&["add", "--all"]);
        fixture.git(&["commit", "-m", "Synthetic blobs"]);
        let commit = fixture.git(&["rev-parse", "HEAD"]);
        let loose = |entry| {
            let oid = fixture.git(&["rev-parse", entry]);
            assets.join(".git/objects").join(&oid[..2]).join(&oid[2..])
        };
        let object = loose("HEAD:SOUL.md");
        fs::remove_file(&object).unwrap();
        fs::copy(loose("HEAD:other"), object).unwrap();
        assert_eq!(
            fixture.git(&["cat-file", "blob", "HEAD:SOUL.md"]),
            "modified"
        );

        let directory = ScopedDirectory::open_absolute(&fixture.0).unwrap();
        assert!(matches!(
            read_asset_blob(&directory, &commit, relative)
                .unwrap_err()
                .code,
            ErrorCode::ObjectUnavailable
        ));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod context_tests {
    use super::*;

    #[test]
    fn context_asset_commit_must_be_clean_and_exact() {
        let root = tempfile::tempdir().unwrap();
        let assets = root.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        let git = |args: &[&str]| {
            let result = isolated_git_command(&assets)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    // Detached maintenance can race with the object-store walk.
                    "-c",
                    "maintenance.auto=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            String::from_utf8(result.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "--quiet"]);
        std::fs::write(assets.join("fixture.md"), b"synthetic asset\n").unwrap();
        git(&["add", "fixture.md"]);
        git(&["commit", "-qm", "Fixture"]);
        let pin = git(&["rev-parse", "HEAD"]);
        let candidate = crate::config_file::ScopedDirectory::open_absolute(root.path()).unwrap();
        let pinned = PinnedAssets::open(&candidate, &pin).unwrap();

        std::fs::write(assets.join("fixture.md"), b"dirty fixture\n").unwrap();
        assert!(matches!(
            pinned.recheck().unwrap_err().code,
            ErrorCode::DirtyConflict
        ));
        git(&["add", "fixture.md"]);
        assert!(matches!(
            pinned.recheck().unwrap_err().code,
            ErrorCode::DirtyConflict
        ));
        git(&["commit", "-qm", "Next"]);
        assert!(matches!(
            pinned.recheck().unwrap_err().code,
            ErrorCode::StaleRevision
        ));

        let next = git(&["rev-parse", "HEAD"]);
        let pinned = PinnedAssets::open(&candidate, &next).unwrap();
        std::fs::write(assets.join("untracked.md"), b"untracked synthetic asset").unwrap();
        assert!(matches!(
            pinned.recheck().unwrap_err().code,
            ErrorCode::DirtyConflict
        ));
        std::fs::remove_file(assets.join("untracked.md")).unwrap();
        git(&["update-index", "--assume-unchanged", "fixture.md"]);
        assert!(matches!(
            pinned.recheck().unwrap_err().code,
            ErrorCode::AuthorityUnverified
        ));
    }
}
