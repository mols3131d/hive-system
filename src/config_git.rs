use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::error::{ErrorCode, HiveError};

const MAX_BLOB_BYTES: usize = 128 * 1024;

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

fn resolve_commit(root: &Path, revision: &str) -> Result<Option<String>, HiveError> {
    resolve_commit_with(&mut git_command(root), revision)
}

fn resolve_commit_with(command: &mut Command, revision: &str) -> Result<Option<String>, HiveError> {
    let expression = format!("{revision}^{{commit}}");
    let result = execute(command.args([
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        &expression,
    ]))?;
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
    read_blob_with(|| git_command(root), revision, relative)
}

#[cfg(unix)]
pub(crate) fn read_asset_blob(
    root: &crate::config_file::ScopedDirectory,
    revision: &str,
    relative: &Path,
) -> Result<Option<Vec<u8>>, HiveError> {
    let git_dir = root
        .open_directory(OsStr::new("assets"))?
        .open_directory(OsStr::new(".git"))?;
    if git_dir.contains(c"commondir")? {
        return Err(HiveError::new(
            ErrorCode::InvalidConfig,
            "The assets directory must have a dedicated Git repository.",
        ));
    }
    read_blob_with(
        || {
            let mut command = git_command(Path::new("."));
            command.arg("--git-dir=.");
            // SAFETY: read_blob_with runs the commands before git_dir is dropped.
            unsafe { git_dir.set_command_directory(&mut command) };
            command
        },
        revision,
        relative,
    )
}

fn read_blob_with(
    command: impl Fn() -> Command,
    revision: &str,
    relative: &Path,
) -> Result<Option<Vec<u8>>, HiveError> {
    if !is_full_oid(revision) {
        return Err(HiveError::new(
            ErrorCode::InvalidArgument,
            "A snapshot requires a full Git commit object ID",
        ));
    }
    let Some(commit) = resolve_commit_with(&mut command(), revision)? else {
        return Err(unavailable_blob());
    };
    if !commit.eq_ignore_ascii_case(revision) {
        return Err(unavailable_blob());
    }

    let path = relative_git_path(relative)?;
    let result = execute(
        command()
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
    let object = tree_blob_oid(&result.stdout, path)?;
    let size = execute(command().args(["cat-file", "-s", object]))?;
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

    let result = execute(command().args(["cat-file", "blob", object]))?;
    if !result.status.success() || result.stdout.len() != size {
        return Err(unavailable_blob());
    }
    Ok(Some(result.stdout))
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

pub(crate) fn status(root: &Path) -> Result<bool, HiveError> {
    let filters = git(
        root,
        [
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(clean|process|required)$",
        ],
    )?;
    if !matches!(filters.status.code(), Some(0 | 1)) {
        return Err(HiveError::new(
            ErrorCode::IoError,
            "Git could not inspect worktree filter configuration",
        ));
    }
    let mut command = git_command(root);
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

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::symlink, time::SystemTime};

    use super::*;
    use crate::config_file::ScopedDirectory;

    #[test]
    fn asset_blob_reads_only_a_bounded_regular_entry_at_the_selected_commit() {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Fixture(
            env::temp_dir().join(format!("hive-asset-test-{}-{nonce}", std::process::id())),
        );
        let assets = root.0.join("assets");
        let relative = Path::new("bots/queen/SOUL.md");
        let soul = assets.join(relative);
        fs::create_dir_all(soul.parent().unwrap()).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .env_clear()
                .env("HOME", &root.0)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("PATH", env::var_os("PATH").unwrap())
                .current_dir(&assets)
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
        };
        let save = || {
            git(&["add", "--all"]);
            git(&["commit", "-m", "Synthetic asset snapshot"]);
            git(&["rev-parse", "HEAD"])
        };
        git(&["init", "--initial-branch=main"]);
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
}
