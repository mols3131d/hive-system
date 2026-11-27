use std::env;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::dolt::{
    DOLT_VERSION, INITIAL_SCHEMA, STORAGE_SCHEMA_VERSION, initial_schema_tables, is_commit_hash,
    table_shape,
};
use crate::error::{ErrorCode, HiveError};

pub(super) struct Installed<'a> {
    dolt: PathBuf,
    git: PathBuf,
    scratch: &'a File,
    home: PathBuf,
}

impl<'a> Installed<'a> {
    pub(super) fn check(scratch: &'a File) -> Result<Self, HiveError> {
        let installed = Self {
            dolt: installed_path("dolt")?,
            git: installed_path("git")?,
            scratch,
            home: super::fd_path(scratch),
        };
        let stdout = installed.dolt(&installed.home, &["version"], None)?;
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
        if !installed
            .git(&installed.home, &["--version"])?
            .starts_with("git version ")
        {
            return Err(HiveError::new(
                ErrorCode::RuntimeUnavailable,
                "The installed Git executable returned an unrecognized version.",
            ));
        }
        Ok(installed)
    }

    pub(super) fn initialize_db(&self, db: &Path, workspace_id: &str) -> Result<String, HiveError> {
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
        if row["branch"] != "main" {
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
        restrict_db_modes(db)?;
        Ok(commit.to_owned())
    }

    pub(super) fn initialize_assets(&self, assets: &Path) -> Result<String, HiveError> {
        self.git(assets, &["init", "--initial-branch=main", "--template="])?;
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
        if commit.len() != 40
            || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.git(assets, &["symbolic-ref", "HEAD"])?.trim() != "refs/heads/main"
            || self.git(assets, &["rev-list", "--count", "HEAD"])?.trim() != "1"
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
        Ok(commit.to_owned())
    }

    fn query(&self, db: &Path, query: &str) -> Result<Vec<Value>, HiveError> {
        #[derive(serde::Deserialize)]
        struct Rows {
            rows: Vec<Value>,
        }
        let stdout = self.dolt(db, &["sql", "-r", "json", "-q", query], None)?;
        serde_json::from_str::<Rows>(&stdout)
            .map(|result| result.rows)
            .map_err(|_| unverified_db())
    }

    fn dolt(&self, cwd: &Path, args: &[&str], input: Option<File>) -> Result<String, HiveError> {
        let mut command = self.command(&self.dolt, cwd);
        command
            .env("DOLT_ROOT_PATH", &self.home)
            .env("DOLT_DISABLE_VERSION_CHECKS", "true")
            .env("DOLT_DISABLE_EVENT_FLUSH", "true")
            .args(args);
        if let Some(input) = input {
            command.stdin(input);
        }
        run(command, "Dolt", args[0])
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Result<String, HiveError> {
        let mut command = self.command(&self.git, cwd);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_DEFAULT_HASH", "sha1")
            .env("GIT_AUTHOR_NAME", "Hive")
            .env("GIT_AUTHOR_EMAIL", "bootstrap@hive.invalid")
            .env("GIT_COMMITTER_NAME", "Hive")
            .env("GIT_COMMITTER_EMAIL", "bootstrap@hive.invalid")
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args);
        run(command, "Git", args[0])
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

// Dolt 2.7.6 explicitly sets broad permissions on some internal files.
// Only touch regular single-link files within the verified private staging tree.
fn restrict_db_modes(db: &Path) -> Result<(), HiveError> {
    fn restrict(directory: &Path) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let meta = entry.metadata()?;
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
