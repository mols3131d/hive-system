use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::dolt::{
    DOLT_VERSION, INITIAL_SCHEMA, STORAGE_SCHEMA_VERSION, initial_schema_tables, is_commit_hash,
    table_shape,
};
use crate::error::{ErrorCode, HiveError};

pub(crate) struct Installed<'a> {
    dolt: PathBuf,
    git: PathBuf,
    scratch: &'a File,
    home: PathBuf,
}

impl<'a> Installed<'a> {
    pub(crate) fn check(scratch: &'a File) -> Result<Self, HiveError> {
        let installed = Self::locate(scratch)?;
        check_dolt_version(&installed.dolt(&installed.home, &["version"], None)?)?;
        if !installed
            .git(&installed.home, &["--version"])?
            .starts_with("git version ")
        {
            return Err(unrecognized_git_version());
        }
        Ok(installed)
    }

    pub(super) fn locate(scratch: &'a File) -> Result<Self, HiveError> {
        Ok(Self {
            dolt: installed_path("dolt")?,
            git: installed_path("git")?,
            scratch,
            home: super::fd_path(scratch),
        })
    }

    pub(super) fn doctor_versions(&self) -> Result<(String, &'static str), HiveError> {
        let mut command = self.dolt_command(&self.home);
        command.arg("version");
        let dolt = check_dolt_version(&run_version(command, "Dolt")?)?;
        let mut command = self.git_command(&self.home);
        command.arg("--version");
        let stdout = run_version(command, "Git")?;
        let git = stdout
            .trim()
            .strip_prefix("git version ")
            .filter(|version| {
                version.len() <= 64
                    && version.split('.').count() >= 3
                    && version.split('.').all(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
            })
            .ok_or_else(unrecognized_git_version)?;
        Ok((git.to_owned(), dolt))
    }

    pub(crate) fn initialize_db(
        &self,
        db: &Path,
        workspace_id: &str,
    ) -> Result<(String, File), HiveError> {
        self.dolt(
            db,
            &[
                "init",
                "--initial-branch",
                "main",
                "--name",
                "Hive",
                "--email",
                "bootstrap@hive.invalid",
            ],
            None,
        )?;

        let native = super::open_directory(&db.join(".dolt"))?;
        let digest = format!("{:x}", Sha256::digest(INITIAL_SCHEMA));
        let mut sql = INITIAL_SCHEMA.to_vec();
        // These interpolations are an internally generated UUID, integer and digest.
        sql.extend_from_slice(format!(
            "\nINSERT INTO workspace_meta VALUES (1, '{workspace_id}');\n\
            INSERT INTO schema_migrations VALUES ({STORAGE_SCHEMA_VERSION}, '{digest}', UTC_TIMESTAMP(6));\n\
            INSERT INTO workspace_counters VALUES ('issue_number', 1);\n"
        ).as_bytes());
        let script = self.home.join("initial.sql");
        fs::write(&script, sql)
            .map_err(|_| super::io_error("Could not stage the initial migration."))?;
        let input = File::open(&script)
            .map_err(|_| super::io_error("Could not open the staged migration."))?;
        self.dolt(db, &["sql"], Some(input))?;
        self.dolt(db, &["add", "."], None)?;
        self.dolt(
            db,
            &[
                "commit",
                "--author",
                "Hive <bootstrap@hive.invalid>",
                "-m",
                "Initialize Hive workspace",
            ],
            None,
        )?;

        let commit = self.verify_db(db, workspace_id, None)?;
        Ok((commit, native))
    }

    pub(crate) fn verify_db(
        &self,
        db: &Path,
        workspace_id: &str,
        expected_commit: Option<&str>,
    ) -> Result<String, HiveError> {
        let head = self.query(
            db,
            "SELECT ACTIVE_BRANCH() AS branch, DOLT_HASHOF('HEAD') AS revision",
        )?;
        let [row] = head.as_slice() else {
            return Err(unverified_db());
        };
        let commit = row["revision"]
            .as_str()
            .filter(|hash| is_commit_hash(hash))
            .ok_or_else(unverified_db)?;
        if row["branch"] != "main" || expected_commit.is_some_and(|expected| commit != expected) {
            return Err(unverified_db());
        }

        let count = |table: &str| -> Result<_, HiveError> {
            self.query(db, &format!("SELECT COUNT(*) AS count FROM {table}"))
        };
        if count("dolt_status")? != [json!({"count": 0})]
            || count("dolt_log")? != [json!({"count": 2})]
        {
            return Err(unverified_db());
        }
        let digest = format!("{:x}", Sha256::digest(INITIAL_SCHEMA));
        for (select, expected) in [
            (
                "singleton_id, workspace_id FROM workspace_meta",
                json!({"singleton_id": 1, "workspace_id": workspace_id}),
            ),
            (
                "version, migration_digest FROM schema_migrations",
                json!({"version": STORAGE_SCHEMA_VERSION, "migration_digest": digest}),
            ),
            (
                "name, next_value FROM workspace_counters",
                json!({"name": "issue_number", "next_value": 1}),
            ),
        ] {
            if self.query(db, &format!("SELECT {select} AS OF '{commit}' LIMIT 2"))? != [expected] {
                return Err(unverified_db());
            }
        }
        self.verify_schema(db, commit)?;
        restrict_db_modes(db)?;
        Ok(commit.to_owned())
    }

    pub(crate) fn verify_schema(&self, db: &Path, commit: &str) -> Result<(), HiveError> {
        let tables = self.query(db, &format!("SHOW FULL TABLES AS OF '{commit}'"))?;
        if tables.len() != initial_schema_tables().count() {
            return Err(unverified_db());
        }
        for (table, digest) in initial_schema_tables() {
            let rows = self.query(db, &format!("SHOW CREATE TABLE {table} AS OF '{commit}'"))?;
            if !matches!(rows.as_slice(), [row] if row["Table"] == table
                && row["Create Table"].as_str().is_some_and(|sql|
                    format!("{:x}", Sha256::digest(table_shape(sql))) == digest))
            {
                return Err(HiveError::new(
                    ErrorCode::SchemaIncompatible,
                    format!(
                        "The initial committed definition of {table} differs from the pinned schema."
                    ),
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn initialize_assets(&self, assets: &Path) -> Result<(String, File), HiveError> {
        self.git(assets, &["init", "--initial-branch=main", "--template="])?;
        let native = super::open_directory(&assets.join(".git"))?;
        self.git(
            assets,
            &[
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "Initialize Hive assets",
            ],
        )?;
        let commit = self.git(
            assets,
            &["rev-parse", "--verify", "refs/heads/main^{commit}"],
        )?;
        let commit = commit.trim();
        self.verify_initial_assets(assets, commit)?;
        Ok((commit.to_owned(), native))
    }

    pub(crate) fn verify_initial_assets(
        &self,
        assets: &Path,
        commit: &str,
    ) -> Result<(), HiveError> {
        if commit.len() != 40
            || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.git(assets, &["symbolic-ref", "HEAD"])?.trim() != "refs/heads/main"
            || self
                .git(assets, &["rev-parse", "--verify", "HEAD^{commit}"])?
                .trim()
                != commit
            || self.git(assets, &["rev-list", "--count", "HEAD"])?.trim() != "1"
            || self
                .git(assets, &["rev-parse", "--verify", "HEAD^{tree}"])?
                .trim()
                != super::EMPTY_GIT_TREE
            || !self.git(assets, &["ls-tree", "HEAD"])?.is_empty()
            || !self
                .git(assets, &["status", "--porcelain", "--untracked-files=all"])?
                .is_empty()
            || !self.git(assets, &["remote"])?.is_empty()
        {
            return Err(HiveError::new(
                ErrorCode::AuthorityUnverified,
                "Could not verify the initial Git asset repository and commit.",
            ));
        }
        Ok(())
    }

    pub(crate) fn query(&self, db: &Path, query: &str) -> Result<Vec<Value>, HiveError> {
        #[derive(serde::Deserialize)]
        struct Rows {
            rows: Vec<Value>,
        }
        let stdout = self.dolt(db, &["sql", "-r", "json", "-q", query], None)?;
        // Dolt 2.7.6 prints an empty object when a SELECT returns zero rows.
        if stdout.trim() == "{}" {
            return Ok(Vec::new());
        }
        serde_json::from_str::<Rows>(&stdout)
            .map(|result| result.rows)
            .map_err(|_| unverified_db())
    }

    pub(crate) fn dolt(
        &self,
        cwd: &Path,
        args: &[&str],
        input: Option<File>,
    ) -> Result<String, HiveError> {
        let mut command = self.dolt_command(cwd);
        command.args(args);
        if let Some(input) = input {
            command.stdin(input);
        }
        run(command, "Dolt", args[0])
    }

    pub(crate) fn prepare_dolt_restore(&self) -> Result<(), HiveError> {
        for (key, value) in [("user.name", "Hive"), ("user.email", "backup@hive.invalid")] {
            self.dolt(
                &self.home,
                &["config", "--global", "--add", key, value],
                None,
            )?;
        }
        Ok(())
    }

    fn dolt_command(&self, cwd: &Path) -> Command {
        let mut command = self.command(&self.dolt, cwd);
        command
            .env("DOLT_ROOT_PATH", &self.home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true");
        command
    }

    pub(crate) fn git(&self, cwd: &Path, args: &[&str]) -> Result<String, HiveError> {
        let mut command = self.git_command(cwd);
        command.args(args);
        run(command, "Git", args[0])
    }

    pub(crate) fn read_committed_soul(
        &self,
        assets: &Path,
        revision: &str,
        path: &str,
    ) -> Result<Vec<u8>, HiveError> {
        crate::config_git::read_committed_asset_blob(
            &|| Ok(self.git_command(assets)),
            revision,
            Path::new(path),
        )?
        .ok_or_else(|| {
            HiveError::new(
                ErrorCode::NotFound,
                "The selected Queen's Soul is missing from the asset commit.",
            )
        })
    }

    pub(crate) fn git_command(&self, cwd: &Path) -> Command {
        let mut command = self.command(&self.git, cwd);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_DEFAULT_HASH", "sha1")
            .env("GIT_DEFAULT_REF_FORMAT", "files")
            .env("GIT_AUTHOR_NAME", "Hive")
            .env("GIT_AUTHOR_EMAIL", "bootstrap@hive.invalid")
            .env("GIT_COMMITTER_NAME", "Hive")
            .env("GIT_COMMITTER_EMAIL", "bootstrap@hive.invalid")
            .args(["-c", "core.hooksPath=/dev/null"]);
        command
    }

    fn command(&self, executable: &Path, cwd: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &self.home)
            .env("TMPDIR", &self.home)
            .env("XDG_CONFIG_HOME", &self.home)
            .env("XDG_DATA_HOME", &self.home)
            .env("XDG_CACHE_HOME", &self.home)
            .env("XDG_STATE_HOME", &self.home)
            .env("XDG_RUNTIME_DIR", &self.home)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null());

        // SAFETY: The closure calls only async-signal-safe Linux operations
        // between fork and exec. Check for a parent exit before prctl took effect.
        let parent = std::process::id() as libc::pid_t;
        let scratch = self.scratch.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                libc::umask(0o077);
                // Keep only our scratch directory across exec. CLOEXEC keeps
                // Rust's exec-error pipe usable until exec replaces the child.
                if libc::syscall(
                    libc::SYS_close_range,
                    3u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) != 0
                    || libc::fcntl(scratch, libc::F_SETFD, 0) == -1
                {
                    return Err(io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
        command
    }
}

fn check_dolt_version(stdout: &str) -> Result<&'static str, HiveError> {
    let version = stdout.trim().strip_prefix("dolt version ").filter(|value| {
        !value.is_empty()
            && value.len() <= 24
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
    });
    if version != Some(DOLT_VERSION) {
        let detected = version.map_or_else(
            || "The executable returned an unrecognized version.".to_owned(),
            |value| format!("Installed version is {value}."),
        );
        return Err(HiveError::new(
            ErrorCode::SchemaIncompatible,
            format!("Workspace initialization requires Dolt {DOLT_VERSION}. {detected}"),
        ));
    }
    Ok(DOLT_VERSION)
}

fn run_version(mut command: Command, engine: &str) -> Result<String, HiveError> {
    const LIMIT: usize = 1024;
    const TIMEOUT: Duration = Duration::from_secs(3);
    let failed = |reason| {
        HiveError::new(
            ErrorCode::RuntimeUnavailable,
            format!("{engine} version probe {reason}."),
        )
    };
    let mut disposition = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: The buffer is valid and a null action only reads the disposition.
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), disposition.as_mut_ptr()) } != 0 {
        return Err(failed("could not inspect child reaping"));
    }
    // SAFETY: sigaction initialized the disposition.
    let disposition = unsafe { disposition.assume_init() };
    if disposition.sa_sigaction != libc::SIG_DFL || disposition.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(failed("requires waitable children"));
    }
    let deadline = Instant::now() + TIMEOUT;
    let mut child = command
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| failed("could not start"))?;
    let leader = child.id() as libc::pid_t;
    let mut stdout = child.stdout.take().expect("version stdout is piped");
    let output = (|| {
        let mut output = Vec::new();
        let mut buffer = [0; 256];
        let mut poll_fd = libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failed("timed out"));
            }
            // SAFETY: poll_fd is initialized and stdout owns the live descriptor.
            let ready = unsafe { libc::poll(&mut poll_fd, 1, remaining.as_millis().max(1) as i32) };
            if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            match ready {
                0 => return Err(failed("timed out")),
                ..0 => return Err(failed("could not read output")),
                _ => {}
            }
            // The pipe has readable bytes or EOF, and there is no other reader.
            let capacity = buffer.len().min(LIMIT + 1 - output.len());
            let count = match stdout.read(&mut buffer[..capacity]) {
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(failed("could not read output")),
            };
            if count == 0 {
                break;
            }
            if output.len() + count > LIMIT {
                return Err(failed("exceeded its output limit"));
            }
            output.extend_from_slice(&buffer[..count]);
        }
        Ok(output)
    })();

    let output = loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: The buffer is valid and leader is the PID returned by spawn.
        // WNOWAIT observes exit without releasing the PID that names our group.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                leader as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // Without waitable-child evidence, do not signal the numeric group.
            return Err(failed("could not verify its child"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if output.is_err() {
            break output;
        }
        if remaining.is_zero() {
            break Err(failed("timed out"));
        }
        // SAFETY: On success waitid returned a child event or left si_pid zero.
        if unsafe { info.assume_init().si_pid() } != 0 {
            break output;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    };

    // SAFETY: spawn placed this probe in a fresh group named by its leader.
    // The waitable leader remains unreaped, reserving that ID until after killpg.
    let signaled = unsafe { libc::killpg(leader, libc::SIGKILL) };
    if signaled != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(failed("could not terminate its process group"));
    }
    let status = child.wait().map_err(|_| failed("could not be reaped"))?;
    let output = output?;
    if !status.success() {
        return Err(failed("failed"));
    }
    String::from_utf8(output).map_err(|_| failed("returned invalid output"))
}

fn unrecognized_git_version() -> HiveError {
    HiveError::new(
        ErrorCode::RuntimeUnavailable,
        "The installed Git executable returned an unrecognized version.",
    )
}

// Dolt 2.7.6 explicitly sets broad permissions on some internal files.
// Only touch regular single-link files within the verified private staging tree.
pub(crate) fn restrict_db_modes(db: &Path) -> Result<(), HiveError> {
    fn restrict(directory: &Path) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.is_dir() {
                fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
                restrict(&entry.path())?;
            } else if meta.is_file() && meta.nlink() == 1 {
                fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600))?;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected Dolt file",
                ));
            }
        }
        Ok(())
    }
    restrict(db).map_err(|_| super::io_error("Could not secure the staged Dolt repository."))
}

fn installed_path(name: &str) -> Result<PathBuf, HiveError> {
    let mut found_unusable = false;
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path).filter(|dir| dir.is_absolute()) {
            let entry = directory.join(name);
            match entry.metadata() {
                Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => {
                    return Ok(entry);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => found_unusable = true,
            }
        }
    }
    let detail = if found_unusable {
        "is present but unusable"
    } else {
        "was not found"
    };
    Err(HiveError::new(
        ErrorCode::RuntimeUnavailable,
        format!("The installed {name} executable {detail} on absolute PATH entries."),
    ))
}

fn run(mut command: Command, engine: &str, step: &str) -> Result<String, HiveError> {
    let output = command.output().map_err(|error| {
        HiveError::new(
            ErrorCode::RuntimeUnavailable,
            format!("Could not execute installed {engine} for {step} ({error})."),
        )
    })?;
    if !output.status.success() {
        return Err(HiveError::new(
            if matches!(step, "version" | "--version") {
                ErrorCode::RuntimeUnavailable
            } else {
                ErrorCode::IoError
            },
            format!("{engine} failed during {step} ({}).", output.status),
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| {
        HiveError::new(
            ErrorCode::IoError,
            format!("{engine} returned invalid output during {step}."),
        )
    })
}

fn unverified_db() -> HiveError {
    HiveError::new(
        ErrorCode::AuthorityUnverified,
        "Could not verify the initial Dolt main commit, ledger, workspace identity, or counter.",
    )
}
