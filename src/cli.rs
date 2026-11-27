use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{
    ArgAction, ArgMatches, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum,
    builder::NonEmptyStringValueParser, error::ErrorKind, value_parser,
};

pub(crate) struct Invocation {
    pub(crate) common: CommonOptions,
    #[allow(
        dead_code,
        reason = "Domain dispatch is introduced after the CLI bootstrap"
    )]
    pub(crate) command: Command,
    pub(crate) command_name: String,
}

pub(crate) enum ParseFailure {
    Display(clap::Error),
    Invalid {
        contract: Contract,
        format: Format,
        command: Option<String>,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Contract {
    #[default]
    #[value(name = "1")]
    V1,
    #[value(name = "2")]
    V2,
}

impl Contract {
    pub(crate) const fn schema_version(self) -> u8 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}

#[derive(Parser)]
#[command(name = "hive", version, about = "Inspect and manage a Hive workspace")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum Format {
    #[default]
    Text,
    Json,
}

impl Format {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

#[derive(Args)]
struct CommonArgs {
    /// Select the CLI contract version.
    #[arg(long, value_enum, value_name = "VERSION")]
    contract: Vec<Contract>,
    /// Select the Hive workspace.
    #[arg(long, value_name = "PATH")]
    workspace: Vec<PathBuf>,
    /// Select the project or reserved scope.
    #[arg(long, value_name = "ID")]
    project: Vec<ProjectId>,
    /// Select the Git project root.
    #[arg(long, value_name = "PATH")]
    project_root: Vec<PathBuf>,
    /// Set the output format.
    #[arg(long, value_enum)]
    format: Vec<Format>,
    /// Use JSON output.
    #[arg(long, action = ArgAction::SetTrue, overrides_with = "json")]
    json: bool,
    /// Read a snapshot at a full local commit object ID.
    #[arg(long, value_name = "COMMIT")]
    at: Vec<CommitOid>,
    /// Check the selected workspace's authority for this read.
    #[arg(long, action = ArgAction::SetTrue, overrides_with = "authority")]
    authority: bool,
}

pub(crate) struct CommonOptions {
    pub(crate) contract: Contract,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) project: Option<ProjectId>,
    pub(crate) project_root: Option<PathBuf>,
    pub(crate) format: Option<Format>,
    pub(crate) view: View,
}

pub(crate) enum View {
    WorkingTree,
    Commit(CommitOid),
    Authority,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProjectId {
    Ordinary(String),
    HiveUserdata,
    General,
}

impl ProjectId {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Ordinary(id) => id,
            Self::HiveUserdata => "_hive-userdata",
            Self::General => "_general",
        }
    }
}

impl FromStr for ProjectId {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "_hive-userdata" => Ok(Self::HiveUserdata),
            "_general" => Ok(Self::General),
            _ => parse_slug(value).map(Self::Ordinary),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TaskId {
    pub(crate) project: Option<ProjectId>,
    #[allow(dead_code, reason = "Task lookup follows the CLI bootstrap")]
    pub(crate) work: String,
}

impl FromStr for TaskId {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (project, work) = match value.split_once('/') {
            Some((project, work)) => (Some(project.parse()?), work),
            None => (None, value),
        };
        Ok(Self {
            project,
            work: parse_slug(work)?,
        })
    }
}

fn parse_slug(value: &str) -> Result<String, &'static str> {
    if is_slug(value) {
        Ok(value.to_owned())
    } else {
        Err("expected a lowercase kebab-case identifier")
    }
}

pub(crate) fn is_slug(value: &str) -> bool {
    value.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommitOid(String);

impl CommitOid {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CommitOid {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err("expected a full hexadecimal Git object ID")
        }
    }
}

fn parse_artifact_id(value: &str) -> Result<PathBuf, &'static str> {
    // Interpret slash-separated record IDs consistently on every platform.
    if value
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
        || value.contains(['\\', ':'])
        || value.chars().any(char::is_control)
    {
        Err("expected a safe relative artifact identifier")
    } else {
        Ok(PathBuf::from(value))
    }
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Inspect, validate, or bind configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// List projects or inspect a project scope.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Read or manage task records.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Read or manage artifacts.
    #[command(subcommand)]
    Artifact(ArtifactCommand),
    /// List or read directives.
    #[command(subcommand)]
    Directive(DirectiveCommand),
    /// List or inspect registered Manager Bots.
    #[command(subcommand)]
    Bot(BotCommand),
    /// List or read scoped Bot memory.
    #[command(subcommand)]
    Memory(MemoryCommand),
    /// Show the selected workspace context.
    Context,
    /// Inspect Issues under contract 2.
    #[command(subcommand, hide = true)]
    Issue(IssueCommand),
    /// Validate workspace records.
    Validate(ValidateArgs),
    /// Check the local environment and workspace.
    Doctor,
}

#[derive(Subcommand)]
#[allow(
    dead_code,
    reason = "Configuration operations follow the CLI bootstrap"
)]
pub(crate) enum ConfigCommand {
    /// Show configuration and its provenance.
    Show(ConfigScopeArgs),
    /// Validate configuration at the selected scope.
    Validate(ConfigScopeArgs),
    /// Create a project binding for an existing workspace scope.
    Bind,
}

#[derive(Args)]
#[allow(
    dead_code,
    reason = "Configuration operations follow the CLI bootstrap"
)]
pub(crate) struct ConfigScopeArgs {
    #[arg(long, value_enum, default_value = "resolved")]
    pub(crate) scope: ConfigScope,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ConfigScope {
    User,
    Project,
    Workspace,
    Resolved,
}

#[derive(Subcommand)]
#[allow(dead_code, reason = "Project operations follow the CLI bootstrap")]
pub(crate) enum ProjectCommand {
    /// List project scope entries.
    List {
        #[arg(long)]
        all_projects: bool,
        #[command(flatten)]
        page: ListArgs,
    },
    /// Show the selected project scope.
    Show {
        #[arg(value_name = "PROJECT-ID")]
        project_id: Option<ProjectId>,
    },
}

#[derive(Args)]
#[allow(dead_code, reason = "List operations follow the CLI bootstrap")]
pub(crate) struct ListArgs {
    #[arg(long, default_value_t = 50, value_parser = value_parser!(u32).range(1..=500))]
    pub(crate) limit: u32,
    #[arg(long, value_name = "KEY", value_parser = NonEmptyStringValueParser::new())]
    pub(crate) after: Option<String>,
}

#[derive(Subcommand)]
#[allow(dead_code, reason = "Task operations follow the CLI bootstrap")]
pub(crate) enum TaskCommand {
    /// List task summaries.
    List {
        #[arg(long, value_enum, default_value = "active")]
        area: TaskArea,
        #[arg(long, value_enum)]
        status: Option<TaskStatus>,
        #[arg(long, value_enum)]
        priority: Option<TaskPriority>,
        #[arg(long)]
        all_projects: bool,
        #[command(flatten)]
        page: ListArgs,
    },
    /// Read a task entrypoint.
    Show { task: TaskId },
    /// Read bounded context for resuming a task.
    Context {
        task: TaskId,
        #[arg(long, value_name = "ID", value_parser = parse_artifact_id)]
        include_artifact: Vec<PathBuf>,
        #[arg(long, default_value_t = 32768, value_parser = value_parser!(u32).range(1..=262144))]
        max_bytes: u32,
    },
    /// Issue a new backlog ticket.
    Create {
        task: TaskId,
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
        #[arg(long, value_name = "REFERENCE", value_parser = NonEmptyStringValueParser::new())]
        project_source: Option<String>,
        #[arg(long, value_name = "OID")]
        expected_revision: Option<CommitOid>,
    },
    /// Move a backlog task to active work.
    Select {
        task: TaskId,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
    },
    /// Acquire the assignment for an active task.
    Claim {
        task: TaskId,
        #[arg(long, value_name = "REFERENCE", value_parser = NonEmptyStringValueParser::new())]
        holder: String,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
    },
    /// Release a task assignment and record continuation.
    Release {
        task: TaskId,
        #[arg(long, value_name = "ID", value_parser = NonEmptyStringValueParser::new())]
        claim_id: String,
        #[arg(long, value_enum)]
        status: WaitingStatus,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
        #[arg(long, value_name = "PATH")]
        note_file: PathBuf,
    },
    /// Transfer a task assignment and record continuation.
    Transfer {
        task: TaskId,
        #[arg(long, value_name = "ID", value_parser = NonEmptyStringValueParser::new())]
        claim_id: String,
        #[arg(long, value_name = "HOLDER", value_parser = NonEmptyStringValueParser::new())]
        to: String,
        #[arg(long, value_enum)]
        status: WaitingStatus,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
        #[arg(long, value_name = "PATH")]
        note_file: PathBuf,
    },
    /// Clear an abandoned assignment using termination evidence.
    Recover {
        task: TaskId,
        #[arg(long, value_name = "ID", value_parser = NonEmptyStringValueParser::new())]
        claim_id: String,
        #[arg(long, value_enum)]
        status: WaitingStatus,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
        #[arg(long, value_name = "PATH")]
        evidence_file: PathBuf,
    },
    /// Record acceptance and close the assignment.
    Accept {
        task: TaskId,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
        #[arg(long, value_name = "PATH")]
        outcome_file: PathBuf,
        #[arg(long, value_name = "ID", value_parser = NonEmptyStringValueParser::new())]
        claim_id: Option<String>,
    },
    /// Move an accepted task to the archive.
    Archive {
        task: TaskId,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
    },
    /// Cancel a task and retain its recorded outcome.
    Cancel {
        task: TaskId,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
        #[arg(long, value_name = "PATH")]
        outcome_file: PathBuf,
        #[arg(long, value_name = "ID", value_parser = NonEmptyStringValueParser::new())]
        claim_id: Option<String>,
    },
    /// Reopen an archived task as active work.
    Reopen {
        task: TaskId,
        #[arg(long, value_name = "OID")]
        expected_revision: CommitOid,
        #[arg(long, value_name = "PATH")]
        note_file: PathBuf,
    },
    /// Update a task's local draft or progress status.
    Update {
        task: TaskId,
        #[arg(long, value_name = "TOKEN", value_parser = NonEmptyStringValueParser::new())]
        if_match: String,
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[arg(long, value_enum)]
        status: Option<ProgressStatus>,
        #[arg(long, value_name = "ID", value_parser = NonEmptyStringValueParser::new())]
        claim_id: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum TaskArea {
    Backlog,
    Active,
    Archive,
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum TaskStatus {
    Todo,
    InProgress,
    Review,
    Done,
    Cancelled,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum TaskPriority {
    Low,
    Normal,
    High,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum WaitingStatus {
    Todo,
    Review,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum ProgressStatus {
    Todo,
    InProgress,
    Review,
}

#[derive(Subcommand)]
#[allow(dead_code, reason = "Artifact operations follow the CLI bootstrap")]
pub(crate) enum ArtifactCommand {
    /// List artifacts for the selected owner.
    List {
        #[command(flatten)]
        owner: ArtifactOwnerArgs,
        #[arg(long, value_enum)]
        kind: Option<ArtifactKind>,
        #[arg(long, value_enum)]
        status: Option<ArtifactStatus>,
        #[command(flatten)]
        page: ListArgs,
    },
    /// Read an artifact for the selected owner.
    Show {
        #[arg(value_parser = parse_artifact_id)]
        id: PathBuf,
        #[command(flatten)]
        owner: ArtifactOwnerArgs,
    },
    /// Add a new artifact for the selected owner.
    Add {
        #[arg(value_parser = parse_artifact_id)]
        id: PathBuf,
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
        #[command(flatten)]
        owner: ArtifactOwnerArgs,
        #[arg(long, value_name = "ID", conflicts_with = "standalone", value_parser = NonEmptyStringValueParser::new())]
        claim_id: Option<String>,
    },
    /// Update an existing artifact for the selected owner.
    Update {
        #[arg(value_parser = parse_artifact_id)]
        id: PathBuf,
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
        #[arg(long, value_name = "TOKEN", value_parser = NonEmptyStringValueParser::new())]
        if_match: String,
        #[command(flatten)]
        owner: ArtifactOwnerArgs,
        #[arg(long, value_name = "ID", conflicts_with = "standalone", value_parser = NonEmptyStringValueParser::new())]
        claim_id: Option<String>,
    },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
#[allow(dead_code, reason = "Artifact operations follow the CLI bootstrap")]
pub(crate) struct ArtifactOwnerArgs {
    #[arg(long, value_name = "TASK")]
    pub(crate) task: Option<TaskId>,
    #[arg(long)]
    pub(crate) standalone: bool,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum ArtifactKind {
    Report,
    Research,
    Review,
    Handoff,
    Message,
    Checkpoint,
    WorkNote,
    Manifest,
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum ArtifactStatus {
    Active,
    Parked,
    Closed,
}

#[derive(Subcommand)]
#[allow(dead_code, reason = "Directive operations follow the CLI bootstrap")]
pub(crate) enum DirectiveCommand {
    /// List directives in the selected scope.
    List {
        #[arg(long, value_enum, default_value = "active")]
        status: DirectiveStatus,
        #[command(flatten)]
        page: ListArgs,
    },
    /// Read a directive in the selected scope.
    Show {
        #[arg(value_parser = parse_slug)]
        id: String,
    },
}

#[derive(Clone, Debug, ValueEnum)]
pub(crate) enum DirectiveStatus {
    Proposed,
    Active,
    Superseded,
    Withdrawn,
    All,
}

#[derive(Subcommand)]
#[allow(dead_code, reason = "Bot operations follow the CLI bootstrap")]
pub(crate) enum BotCommand {
    /// List registered Manager Bots.
    List {
        #[command(flatten)]
        page: ListArgs,
    },
    /// Show a registered Bot's identity and routing metadata.
    Show {
        #[arg(value_name = "BOT-ID", value_parser = parse_slug)]
        bot_id: String,
    },
}

#[derive(Subcommand)]
#[allow(dead_code, reason = "Memory operations follow the CLI bootstrap")]
pub(crate) enum MemoryCommand {
    /// List a Bot's scoped memory references.
    List {
        #[arg(value_name = "BOT-ID", value_parser = parse_slug)]
        bot_id: String,
        #[command(flatten)]
        page: ListArgs,
    },
    /// Read the Bot's scoped memory or one topic.
    Show {
        #[arg(value_name = "BOT-ID", value_parser = parse_slug)]
        bot_id: String,
        #[arg(value_parser = parse_slug)]
        topic: Option<String>,
    },
}

#[derive(Args)]
pub(crate) struct DoltTargetArgs {
    /// Expected identity of the local Dolt workspace.
    #[arg(long, value_name = "UUID")]
    pub(crate) workspace_id: Option<String>,
    /// Explicit local Dolt database name.
    #[arg(long, value_name = "NAME")]
    pub(crate) database: Option<String>,
    /// Explicit local Dolt SQL Server port.
    #[arg(long, value_name = "PORT", allow_negative_numbers = true)]
    pub(crate) port: Option<OsString>,
}

#[derive(Subcommand)]
pub(crate) enum IssueCommand {
    /// Show an Issue and its bounded direct relation IDs.
    Show(IssueShowArgs),
}

#[derive(Args)]
pub(crate) struct IssueShowArgs {
    /// Canonical lowercase UUIDv7 or quoted #number Issue selector.
    #[arg(value_name = "ISSUE")]
    pub(crate) issue: String,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct ValidateArgs {
    #[arg(long, value_name = "TASK", conflicts_with = "all_projects")]
    pub(crate) task: Option<TaskId>,
    #[arg(long)]
    pub(crate) all_projects: bool,
    #[arg(long, value_name = "BASE-OID")]
    pub(crate) against: Option<CommitOid>,
    /// Validate the whole workspace with contract 2.
    #[arg(long)]
    pub(crate) all: bool,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

pub(crate) fn parse() -> Result<Invocation, ParseFailure> {
    let args: Vec<_> = std::env::args_os().collect();
    let cli = Cli::command();
    // Global clap arguments propagate values across levels. Separate declarations
    // retain every occurrence at the root and at the selected leaf.
    let mut cli = CommonArgs::augment_args(cli.mut_subcommands(add_leaf_options));
    let matches = match cli.clone().try_get_matches_from(&args) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Err(ParseFailure::Display(error));
        }
        Err(error) => {
            // Use whatever clap recognized before the error. This keeps option
            // values and positionals that resemble switches out of selection.
            let partial = cli
                .clone()
                .ignore_errors(true)
                .try_get_matches_from(&args)
                .ok();
            let root = partial.as_ref();
            let matched = root.and_then(|args| matched_leaf(&cli, args));
            let leaf = matched.as_ref().map(|(_, args)| *args);
            let format = requested_format(root, leaf);
            let contract = requested_contract(root, leaf);
            let message = safe_parse_message(error.kind());
            let message = if format == Format::Text {
                cli.build();
                format!("{message}\n\n{}", usage_for(&mut cli, root))
            } else {
                message.to_owned()
            };
            return Err(ParseFailure::Invalid {
                contract,
                format,
                command: matched.map(|(name, _)| name),
                message,
            });
        }
    };

    let matched = matched_leaf(&cli, &matches);
    let leaf = matched.as_ref().map(|(_, args)| *args);
    let format = requested_format(Some(&matches), leaf);
    let contract = requested_contract(Some(&matches), leaf);
    let (command_name, leaf) = matched.ok_or_else(|| ParseFailure::Invalid {
        contract,
        format,
        command: None,
        message: "A command is required.".to_owned(),
    })?;
    let parsed = || -> Result<_, &'static str> {
        let root = CommonArgs::from_arg_matches(&matches)
            .map_err(|error| safe_parse_message(error.kind()))?;
        let leaf =
            CommonArgs::from_arg_matches(leaf).map_err(|error| safe_parse_message(error.kind()))?;
        let common = root.resolve(leaf)?;
        let Cli { command } =
            Cli::from_arg_matches(&matches).map_err(|error| safe_parse_message(error.kind()))?;
        command.validate(&common)?;
        Ok((common, command))
    };
    match parsed() {
        Ok((common, command)) => Ok(Invocation {
            common,
            command,
            command_name,
        }),
        Err(message) => Err(ParseFailure::Invalid {
            contract,
            format,
            command: Some(command_name),
            message: message.to_owned(),
        }),
    }
}

fn add_leaf_options(command: clap::Command) -> clap::Command {
    if command.has_subcommands() {
        command.mut_subcommands(add_leaf_options)
    } else {
        CommonArgs::augment_args(command)
    }
}

fn matched_leaf<'a>(
    cli: &clap::Command,
    matches: &'a ArgMatches,
) -> Option<(String, &'a ArgMatches)> {
    let (first, subcommand) = matches.subcommand()?;
    if let Some((second, leaf)) = subcommand.subcommand() {
        Some((format!("{first} {second}"), leaf))
    } else if cli
        .find_subcommand(first)
        .is_some_and(|command| !command.has_subcommands())
    {
        // A partial parse can stop in a domain before selecting a leaf.
        Some((first.to_owned(), subcommand))
    } else {
        None
    }
}

fn usage_for(cli: &mut clap::Command, matches: Option<&ArgMatches>) -> clap::builder::StyledStr {
    if let Some((name, args)) = matches.and_then(ArgMatches::subcommand) {
        let command = cli
            .find_subcommand_mut(name)
            .expect("matched CLI subcommand");
        return usage_for(command, Some(args));
    }
    cli.render_usage()
}

fn requested_format(root: Option<&ArgMatches>, leaf: Option<&ArgMatches>) -> Format {
    let requested_json = |args: &ArgMatches| {
        args.try_get_one::<bool>("json").ok().flatten() == Some(&true)
            || args
                .try_get_many::<Format>("format")
                .ok()
                .flatten()
                .is_some_and(|mut values| values.any(|value| *value == Format::Json))
    };
    if root.into_iter().chain(leaf).any(requested_json) {
        Format::Json
    } else {
        Format::Text
    }
}

fn requested_contract(root: Option<&ArgMatches>, leaf: Option<&ArgMatches>) -> Contract {
    root.into_iter()
        .chain(leaf)
        .filter_map(|args| args.try_get_many::<Contract>("contract").ok().flatten())
        .flatten()
        .copied()
        .next()
        .unwrap_or_default()
}

fn safe_parse_message(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => {
            "An argument or command is not recognized."
        }
        ErrorKind::MissingRequiredArgument
        | ErrorKind::MissingSubcommand
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            "A required argument or command is missing."
        }
        ErrorKind::ArgumentConflict => "Arguments conflict.",
        _ => "An argument has a missing or invalid value.",
    }
}

impl CommonArgs {
    fn resolve(self, leaf: Self) -> Result<CommonOptions, &'static str> {
        let CommonArgs {
            contract,
            workspace,
            project,
            project_root,
            format,
            json,
            at,
            authority,
        } = self;
        let format = one_value(format, leaf.format, "Conflicting --format values.")?;
        let format = match (json || leaf.json, format) {
            (true, Some(Format::Text)) => return Err("--json conflicts with --format text."),
            (true, _) => Some(Format::Json),
            (false, format) => format,
        };
        let at = one_value(at, leaf.at, "Conflicting --at values.")?;
        let view = match (authority || leaf.authority, at) {
            (true, Some(_)) => return Err("--at conflicts with --authority."),
            (true, None) => View::Authority,
            (false, Some(commit)) => View::Commit(commit),
            (false, None) => View::WorkingTree,
        };
        Ok(CommonOptions {
            contract: one_value(contract, leaf.contract, "Conflicting --contract values.")?
                .unwrap_or_default(),
            workspace: one_path(workspace, leaf.workspace, "Conflicting --workspace values.")?,
            project: one_value(project, leaf.project, "Conflicting --project values.")?,
            project_root: one_path(
                project_root,
                leaf.project_root,
                "Conflicting --project-root values.",
            )?,
            format,
            view,
        })
    }
}

fn one_value<T: Eq>(
    root: impl IntoIterator<Item = T>,
    leaf: impl IntoIterator<Item = T>,
    conflict: &'static str,
) -> Result<Option<T>, &'static str> {
    let mut values = root.into_iter().chain(leaf);
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.any(|value| value != first) {
        Err(conflict)
    } else {
        Ok(Some(first))
    }
}

fn one_path(
    root: Vec<PathBuf>,
    leaf: Vec<PathBuf>,
    conflict: &'static str,
) -> Result<Option<PathBuf>, &'static str> {
    // Compare the supplied spellings without normalizing path syntax.
    one_value(
        root.into_iter().map(PathBuf::into_os_string),
        leaf.into_iter().map(PathBuf::into_os_string),
        conflict,
    )
    .map(|value: Option<OsString>| value.map(PathBuf::from))
}

impl CommonOptions {
    fn require_working_tree(&self) -> Result<(), &'static str> {
        if matches!(self.view, View::WorkingTree) {
            Ok(())
        } else {
            Err("--at and --authority are read-only options.")
        }
    }

    fn check_project(&self, selected: Option<&ProjectId>) -> Result<(), &'static str> {
        match (self.project.as_ref(), selected) {
            (Some(explicit), Some(selected)) if explicit != selected => {
                Err("The selected scope conflicts with --project.")
            }
            _ => Ok(()),
        }
    }

    fn check_task(&self, task: Option<&TaskId>) -> Result<(), &'static str> {
        self.check_project(task.and_then(|task| task.project.as_ref()))
    }
}

impl Command {
    fn validate(&self, common: &CommonOptions) -> Result<(), &'static str> {
        if common.contract == Contract::V2 {
            return match self {
                Self::Validate(args) => args.validate_dolt_selection(common),
                Self::Issue(IssueCommand::Show(args)) => args.validate_dolt_selection(common),
                _ => Ok(()),
            };
        }
        match self {
            Self::Config(ConfigCommand::Bind) => {
                common.require_working_tree()?;
                if common.workspace.is_none() || common.project.is_none() {
                    return Err("config bind requires explicit --workspace and --project.");
                }
                Ok(())
            }
            Self::Doctor if matches!(common.view, View::Commit(_)) => {
                Err("doctor does not accept --at.")
            }
            Self::Project(ProjectCommand::Show { project_id }) => {
                common.check_project(project_id.as_ref())
            }
            Self::Issue(_) => Err("Issue commands require --contract 2."),
            Self::Validate(args) => {
                if args.all || args.target.has_any() {
                    return Err("Dolt validation requires --contract 2.");
                }
                if args.against.is_some() && matches!(common.view, View::Authority) {
                    return Err("validate --against does not accept --authority.");
                }
                common.check_task(args.task.as_ref())
            }
            Self::Task(command) => command.validate(common),
            Self::Artifact(command) => command.validate(common),
            _ => Ok(()),
        }
    }
}

impl DoltTargetArgs {
    fn has_any(&self) -> bool {
        self.workspace_id.is_some() || self.database.is_some() || self.port.is_some()
    }

    fn validate_common(
        &self,
        common: &CommonOptions,
        error: &'static str,
    ) -> Result<(), &'static str> {
        if common.workspace.is_some()
            || common.project.is_some()
            || common.project_root.is_some()
            || !matches!(common.view, View::WorkingTree)
        {
            return Err(error);
        }
        Ok(())
    }

    fn has_all(&self) -> bool {
        self.workspace_id.is_some() && self.database.is_some() && self.port.is_some()
    }
}

impl ValidateArgs {
    fn validate_dolt_selection(&self, common: &CommonOptions) -> Result<(), &'static str> {
        const ERROR: &str =
            "Dolt validation accepts only an explicit local database and whole-workspace scope.";
        if self.task.is_some() || self.all_projects || self.against.is_some() {
            return Err(ERROR);
        }
        self.target.validate_common(common, ERROR)?;
        if !self.all || !self.target.has_all() {
            return Err("Dolt validation requires --all, --workspace-id, --database, and --port.");
        }
        Ok(())
    }
}

impl IssueShowArgs {
    fn validate_dolt_selection(&self, common: &CommonOptions) -> Result<(), &'static str> {
        self.target.validate_common(
            common,
            "Dolt issue show accepts only an explicit local database and workspace ID.",
        )?;
        if !self.target.has_all() {
            return Err("Dolt issue show requires --workspace-id, --database, and --port.");
        }
        Ok(())
    }
}

impl TaskCommand {
    fn validate(&self, common: &CommonOptions) -> Result<(), &'static str> {
        match self {
            Self::List {
                area,
                status,
                priority,
                ..
            } => match (area, status, priority) {
                (TaskArea::Backlog, Some(_), _) => Err("Backlog tasks do not have a status."),
                (TaskArea::Archive, _, Some(_)) => Err("Archived tasks do not have a priority."),
                (TaskArea::Active, Some(TaskStatus::Cancelled), _) => {
                    Err("Cancelled is not an active task status.")
                }
                (
                    TaskArea::Archive,
                    Some(TaskStatus::Todo | TaskStatus::InProgress | TaskStatus::Review),
                    _,
                ) => Err("Archived task status must be done or cancelled."),
                _ => Ok(()),
            },
            Self::Show { task } | Self::Context { task, .. } => common.check_task(Some(task)),
            Self::Update {
                task, file, status, ..
            } => {
                common.require_working_tree()?;
                common.check_task(Some(task))?;
                if file.is_none() && status.is_none() {
                    return Err("task update requires --file or --status.");
                }
                Ok(())
            }
            Self::Create { task, .. }
            | Self::Select { task, .. }
            | Self::Claim { task, .. }
            | Self::Release { task, .. }
            | Self::Transfer { task, .. }
            | Self::Recover { task, .. }
            | Self::Accept { task, .. }
            | Self::Archive { task, .. }
            | Self::Cancel { task, .. }
            | Self::Reopen { task, .. } => {
                common.require_working_tree()?;
                common.check_task(Some(task))
            }
        }
    }
}

impl ArtifactCommand {
    fn validate(&self, common: &CommonOptions) -> Result<(), &'static str> {
        match self {
            Self::List { owner, .. } | Self::Show { owner, .. } => {
                common.check_task(owner.task.as_ref())
            }
            Self::Add { owner, .. } | Self::Update { owner, .. } => {
                common.require_working_tree()?;
                common.check_task(owner.task.as_ref())
            }
        }
    }
}
