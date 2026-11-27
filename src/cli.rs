mod conversation_v2;
mod project_v2;

use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{
    Arg, ArgAction, ArgMatches, Args, CommandFactory, FromArgMatches, Parser, Subcommand,
    ValueEnum,
    builder::{
        NonEmptyStringValueParser, OsStringValueParser, StringValueParser, TypedValueParser,
    },
    error::{ContextKind, ContextValue, ErrorKind},
    parser::ValueSource,
    value_parser,
};

pub(crate) use conversation_v2::ConversationListArgs;
pub(crate) use project_v2::ProjectRead;

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
    #[command(skip)]
    ProjectRead(ProjectRead),
    /// Read or manage task records.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Read or manage artifacts.
    #[command(subcommand)]
    Artifact(ArtifactCommand),
    #[command(skip)]
    DoltArtifact(DoltArtifactCommand),
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
    /// Inspect Areas under contract 2.
    #[command(subcommand, hide = true)]
    Area(AreaCommand),
    /// Inspect Issues under contract 2.
    #[command(subcommand, hide = true)]
    Issue(IssueCommand),
    #[command(skip)]
    Resource(ResourceCommand),
    #[command(skip)]
    Label(LabelCommand),
    /// Inspect preserved Queen conversations under contract 2.
    #[command(subcommand, hide = true)]
    Conversation(ConversationCommand),
    #[command(skip)]
    ConversationList(ConversationListArgs),
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
    Show(ConfigShowArgs),
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

#[derive(Args)]
pub(crate) struct ConfigShowArgs {
    #[command(flatten)]
    pub(crate) target: ConfigScopeArgs,
    #[arg(skip)]
    pub(crate) area: Option<String>,
    #[arg(skip)]
    pub(crate) resource: Option<PathBuf>,
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

pub(crate) enum DoltArtifactCommand {
    List(ArtifactListArgs),
    Show(ArtifactShowArgs),
}

#[derive(Args)]
pub(crate) struct ArtifactListArgs {
    /// Select one direct scope as area:UUID, project:UUID, issue:UUID, or resource:UUID.
    #[arg(long, value_name = "KIND:ID", allow_negative_numbers = true)]
    pub(crate) scope: Option<String>,
    /// Page size (default 25, maximum 100).
    #[arg(long, default_value = "25", allow_negative_numbers = true)]
    pub(crate) limit: String,
    /// Continue from the previous page's next_cursor.
    #[arg(long, allow_negative_numbers = true)]
    pub(crate) cursor: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct ArtifactShowArgs {
    /// Exact Artifact UUIDv7.
    #[arg(value_name = "ID", allow_negative_numbers = true)]
    pub(crate) id: String,
    /// Cover a direct link as KIND:ID. Repeat for every linked scope.
    #[arg(long, value_name = "KIND:ID", allow_negative_numbers = true)]
    pub(crate) scope: Vec<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
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
pub(crate) enum DirectiveCommand {
    /// List directives in the selected scope.
    List {
        #[arg(long, value_enum, default_value = "active")]
        status: DirectiveStatus,
        #[command(flatten)]
        page: ListArgs,
        #[arg(long, hide = true)]
        cursor: Option<String>,
        #[command(flatten)]
        selection: DirectiveSelection,
    },
    /// Read a directive in the selected scope.
    Show {
        #[arg(value_parser = parse_slug)]
        id: String,
        #[command(flatten)]
        selection: DirectiveSelection,
    },
}

#[derive(Args)]
pub(crate) struct DirectiveSelection {
    #[arg(long, hide = true)]
    pub(crate) workspace_scope: bool,
    #[arg(long, hide = true, allow_negative_numbers = true)]
    pub(crate) area: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
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
pub(crate) enum BotCommand {
    /// List registered Manager Bots.
    List {
        #[command(flatten)]
        page: ListArgs,
        /// Contract 2 registry filter (active by default, retired, or all).
        #[arg(long, hide = true)]
        status: Option<String>,
        /// Contract 2 continuation from next_cursor.
        #[arg(long, hide = true)]
        cursor: Option<String>,
        #[command(flatten)]
        target: DoltTargetArgs,
    },
    /// Show a registered Bot's identity and routing metadata.
    Show {
        #[arg(value_name = "BOT-ID", value_parser = parse_slug)]
        bot_id: String,
        /// Inspect the Soul at this full asset Git commit (contract 2, requires --workspace).
        #[arg(long, hide = true)]
        asset_commit: Option<String>,
        #[command(flatten)]
        target: DoltTargetArgs,
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
pub(crate) enum ConversationCommand {
    /// Show one preserved Queen conversation.
    Show(ConversationShowArgs),
}

#[derive(Args)]
pub(crate) struct ConversationShowArgs {
    /// Canonical lowercase UUIDv7 of the registered Queen.
    #[arg(long, value_name = "UUID", allow_negative_numbers = true)]
    pub(crate) bot: Option<OsString>,
    /// Canonical lowercase UUIDv7 of the preserved conversation.
    #[arg(long, value_name = "UUID", allow_negative_numbers = true)]
    pub(crate) id: Option<OsString>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Subcommand)]
pub(crate) enum AreaCommand {
    /// List committed root Areas or the direct children of one Area.
    List(AreaListArgs),
    /// Show the committed Area hierarchy or one subtree.
    Tree(AreaTreeArgs),
    /// Show one committed Area.
    Show(AreaShowArgs),
}

#[derive(Args)]
pub(crate) struct AreaListArgs {
    /// Select the direct children of this canonical lowercase UUIDv7.
    #[arg(long, value_name = "ID", allow_negative_numbers = true)]
    pub(crate) parent: Option<String>,
    /// Page size (default 25, maximum 100).
    #[arg(long, default_value = "25", allow_negative_numbers = true)]
    pub(crate) limit: String,
    /// Continue from the previous page's next_cursor.
    #[arg(long)]
    pub(crate) cursor: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct AreaTreeArgs {
    /// Select the subtree rooted at this canonical lowercase UUIDv7.
    #[arg(long, value_name = "ID", allow_negative_numbers = true)]
    pub(crate) root: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct AreaShowArgs {
    /// Canonical lowercase UUIDv7 Area selector.
    #[arg(value_name = "ID", allow_negative_numbers = true)]
    pub(crate) area: String,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Subcommand)]
pub(crate) enum ResourceCommand {
    /// List committed Resource metadata in one explicit scope.
    List(ResourceListArgs),
    /// Show one committed Resource and its stored URI.
    Show(ResourceShowArgs),
}

#[derive(Args)]
pub(crate) struct ResourceListArgs {
    /// Select Resources across the workspace.
    #[arg(long)]
    pub(crate) all: bool,
    /// Select Resources directly linked to this Area UUIDv7.
    #[arg(long, value_name = "ID", allow_negative_numbers = true)]
    pub(crate) area: Option<String>,
    /// Select Resources directly linked to this Issue UUIDv7.
    #[arg(long, value_name = "ID", allow_negative_numbers = true)]
    pub(crate) issue: Option<String>,
    /// Page size (default 25, maximum 100).
    #[arg(long, default_value = "25", allow_negative_numbers = true)]
    pub(crate) limit: String,
    /// Continue from the previous page's next_cursor.
    #[arg(long)]
    pub(crate) cursor: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct ResourceShowArgs {
    /// Canonical lowercase UUIDv7 Resource selector.
    #[arg(value_name = "ID", allow_negative_numbers = true)]
    pub(crate) resource: String,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Subcommand)]
pub(crate) enum LabelCommand {
    /// List committed Labels in one explicit scope.
    List(LabelListArgs),
}

#[derive(Args)]
pub(crate) struct LabelListArgs {
    /// Select all workspace Labels, including unlinked Labels.
    #[arg(long)]
    pub(crate) all: bool,
    /// Select Labels on Issues owned directly by this Area UUIDv7.
    #[arg(long, value_name = "ID", allow_negative_numbers = true)]
    pub(crate) area: Option<String>,
    /// Select Labels linked directly to this Issue UUIDv7.
    #[arg(long, value_name = "ID", allow_negative_numbers = true)]
    pub(crate) issue: Option<String>,
    /// Page size (default 25, maximum 100).
    #[arg(long, default_value = "25", allow_negative_numbers = true)]
    pub(crate) limit: String,
    /// Continue from the previous page's next_cursor.
    #[arg(long, allow_negative_numbers = true)]
    pub(crate) cursor: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Subcommand)]
pub(crate) enum IssueCommand {
    /// Show an Issue and its bounded direct relation IDs.
    Show(IssueShowArgs),
    /// List committed Issue summaries under contract 2.
    List(IssueListArgs),
    /// List committed executable todo Issues that are ready.
    Ready(IssueListArgs),
    /// List committed executable todo Issues with readiness blockers.
    Blocked(IssueListArgs),
}

#[derive(Args)]
pub(crate) struct IssueListArgs {
    /// Select Issues across the workspace.
    #[arg(long)]
    pub(crate) all: bool,
    /// Select one exact Area UUIDv7.
    #[arg(long, value_name = "ID", allow_negative_numbers = true)]
    pub(crate) area: Option<String>,
    /// Filter by exact Issue status.
    #[arg(long)]
    pub(crate) status: Option<String>,
    /// Filter by exact Issue type.
    #[arg(long = "type", value_name = "TYPE")]
    pub(crate) kind: Option<String>,
    /// Filter by priority (0–3).
    #[arg(long, allow_negative_numbers = true)]
    pub(crate) priority: Option<String>,
    /// Filter by exact Label name.
    #[arg(long)]
    pub(crate) label: Option<String>,
    /// Page size (default 25, maximum 100).
    #[arg(long, default_value = "25", allow_negative_numbers = true)]
    pub(crate) limit: String,
    /// Continue from the previous page's next_cursor.
    #[arg(long)]
    pub(crate) cursor: Option<String>,
    #[command(flatten)]
    pub(crate) target: DoltTargetArgs,
}

#[derive(Args)]
pub(crate) struct IssueShowArgs {
    /// Canonical lowercase UUIDv7 or quoted #number Issue selector.
    #[arg(value_name = "ISSUE", allow_negative_numbers = true)]
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
    // Global clap arguments propagate values across levels. Separate declarations
    // retain every occurrence at the root and at the selected leaf.
    let mut cli = CommonArgs::augment_args(Cli::command().mut_subcommands(add_leaf_options));
    for domain in ["bot", "directive"] {
        cli = cli.mut_subcommand(domain, |command| {
            command.mut_subcommands(|leaf| {
                leaf.mut_args(|arg| match arg.get_id().as_str() {
                    "workspace_id" | "database" | "port" => arg.hide(true),
                    _ => arg,
                })
            })
        });
    }
    if let Some(result) = parse_dolt_artifact(&cli, &args) {
        return result;
    }
    if let Some(conversation) = conversation_v2::parse(&cli, &args) {
        return conversation;
    }
    if let Some(project) = project_v2::parse(&cli, &args) {
        return project;
    }
    let mut config_error_matches = None;
    let extension = resource_parser(&cli, &args).or_else(|| label_parser(&cli, &args));
    let (parsed, extension_matches) = if let Some(extension) = extension {
        cli = extension.cli;
        (extension.parsed, extension.partial)
    } else {
        (
            cli.clone().try_get_matches_from(&args).or_else(|error| {
                match error.kind() {
                    ErrorKind::ValueValidation => retry_issue_query(&cli, &args)
                        .map(Ok)
                        .or_else(|| retry_config_show(&cli, &args, &mut config_error_matches)),
                    ErrorKind::UnknownArgument => {
                        retry_config_show(&cli, &args, &mut config_error_matches)
                    }
                    _ => None,
                }
                .unwrap_or(Err(error))
            }),
            None,
        )
    };
    let matches = match parsed {
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
            // Only use output options recognized by Clap, including on retry.
            // Values and positionals that resemble switches stay literal.
            let area_matches = match error.kind() {
                ErrorKind::UnknownArgument | ErrorKind::ValueValidation => {
                    retry_area_read(&cli, &args)
                }
                _ => None,
            };
            let partial = extension_matches
                .or(config_error_matches)
                .or(area_matches)
                .or_else(|| retry_scoped_read_failure(&cli, &args, &error, "bot"))
                .or_else(|| retry_scoped_read_failure(&cli, &args, &error, "directive"))
                .or_else(|| retry_conversation_failure(&cli, &args, error.kind(), "show"))
                .or_else(|| {
                    cli.clone()
                        .ignore_errors(true)
                        .try_get_matches_from(&args)
                        .ok()
                });
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
        let leaf_options =
            CommonArgs::from_arg_matches(leaf).map_err(|error| safe_parse_message(error.kind()))?;
        let common = root.resolve(leaf_options)?;
        let mut command = if let Some(("resource", resource)) = matches.subcommand() {
            Command::Resource(
                ResourceCommand::from_arg_matches(resource)
                    .map_err(|error| safe_parse_message(error.kind()))?,
            )
        } else if let Some(("label", label)) = matches.subcommand() {
            Command::Label(
                LabelCommand::from_arg_matches(label)
                    .map_err(|error| safe_parse_message(error.kind()))?,
            )
        } else {
            Cli::from_arg_matches(&matches)
                .map_err(|error| safe_parse_message(error.kind()))?
                .command
        };
        if common.contract == Contract::V2
            && let Command::Bot(BotCommand::List { page, .. })
            | Command::Directive(DirectiveCommand::List { page, .. }) = &mut command
            && leaf.value_source("limit") == Some(clap::parser::ValueSource::DefaultValue)
        {
            page.limit = 25;
        }
        if let Command::Config(ConfigCommand::Show(args)) = &mut command {
            args.area = leaf.try_get_one::<String>("area").ok().flatten().cloned();
            args.resource = leaf
                .try_get_one::<PathBuf>("resource")
                .ok()
                .flatten()
                .cloned();
        }
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

fn parse_dolt_artifact(
    cli: &clap::Command,
    args: &[OsString],
) -> Option<Result<Invocation, ParseFailure>> {
    if !args.iter().any(|arg| arg == "artifact") {
        return None;
    }
    let v2 = cli.clone().mut_subcommand("artifact", |artifact| {
        artifact
            .mut_subcommand("list", |_| {
                CommonArgs::augment_args(ArtifactListArgs::augment_args(
                    clap::Command::new("list")
                        .about("List committed Artifact metadata in one scope."),
                ))
            })
            .mut_subcommand("show", |_| {
                CommonArgs::augment_args(ArtifactShowArgs::augment_args(
                    clap::Command::new("show")
                        .about("Read one committed Artifact with explicit scope coverage."),
                ))
            })
    });

    // Recover known malformed options to locate the contract and output format.
    // Only the strict parser can supply arguments to dispatch.
    let recover_value = |arg: clap::Arg| {
        arg.value_parser(OsStringValueParser::new())
            .action(ArgAction::Append)
            .allow_negative_numbers(true)
            .num_args(0..=1)
    };
    let recover_common = |command: clap::Command| {
        command.mut_args(|arg| match arg.get_id().as_str() {
            "project" | "workspace" | "at" | "project_root" => recover_value(arg),
            _ => arg,
        })
    };
    let probe = recover_common(v2.clone()).mut_subcommand("artifact", |artifact| {
        artifact.mut_subcommands(|leaf| {
            if !matches!(leaf.get_name(), "list" | "show") {
                return leaf;
            }
            let list = leaf.get_name() == "list";
            let leaf = recover_common(leaf)
                .disable_help_flag(true)
                .arg(
                    clap::Arg::new("help")
                        .long("help")
                        .short('h')
                        .action(ArgAction::SetTrue),
                )
                .mut_args(|arg| match arg.get_id().as_str() {
                    "database" | "workspace_id" | "port" | "scope" | "limit" | "cursor" => {
                        recover_value(arg)
                    }
                    "id" => arg
                        .value_parser(OsStringValueParser::new())
                        .required(false)
                        .num_args(0..),
                    _ => arg,
                })
                .arg(
                    clap::Arg::new("standalone")
                        .long("standalone")
                        .action(ArgAction::SetTrue)
                        .overrides_with("standalone"),
                );
            let leaf = [
                "task", "kind", "status", "after", "file", "actor", "approval",
            ]
            .into_iter()
            .fold(leaf, |leaf, name| {
                leaf.arg(recover_value(clap::Arg::new(name).long(name)))
            });
            if list {
                leaf.arg(
                    clap::Arg::new("extra")
                        .value_parser(OsStringValueParser::new())
                        .num_args(0..),
                )
            } else {
                leaf
            }
        })
    });
    let (matched, recovery_error) = match probe.clone().try_get_matches_from(args) {
        Ok(matches) => (matches, None),
        Err(error) if error.kind() == ErrorKind::UnknownArgument => {
            let partial = probe
                .clone()
                .ignore_errors(true)
                .try_get_matches_from(args)
                .ok()?;
            // Keep probe-only positionals from absorbing a detached value.
            let retry = probe.mut_subcommand("artifact", |artifact| {
                artifact
                    .mut_subcommand("list", |list| list.mut_arg("extra", |arg| arg.last(true)))
                    .mut_subcommand("show", |show| show.mut_arg("id", |arg| arg.num_args(0..=1)))
            });
            let matches =
                recover_unknown_long(&retry, args, &partial, &error, "artifact").unwrap_or(partial);
            (matches, Some(error.kind()))
        }
        Err(_) => return None,
    };
    let (command_name, leaf) = matched_leaf(&v2, &matched)?;
    if !matches!(command_name.as_str(), "artifact list" | "artifact show")
        || requested_contract(Some(&matched), Some(leaf)) != Contract::V2
    {
        return None;
    }
    let format = requested_format(Some(&matched), Some(leaf));
    let invalid = |message: &str| ParseFailure::Invalid {
        contract: Contract::V2,
        format,
        command: Some(command_name.clone()),
        message: message.to_owned(),
    };

    if let Some(error) = recovery_error {
        return Some(Err(invalid(safe_parse_message(error))));
    }

    Some((|| {
        let matches = v2.try_get_matches_from(args).map_err(|error| {
            if error.kind() == ErrorKind::DisplayHelp {
                ParseFailure::Display(error)
            } else {
                invalid(safe_parse_message(error.kind()))
            }
        })?;
        let (_, leaf) = matched_leaf(cli, &matches).expect("matched Artifact read");
        let common = CommonArgs::from_arg_matches(&matches)
            .and_then(|root| CommonArgs::from_arg_matches(leaf).map(|options| (root, options)))
            .map_err(|error| invalid(safe_parse_message(error.kind())))?;
        let common = common.0.resolve(common.1).map_err(invalid)?;
        let command = if command_name == "artifact list" {
            ArtifactListArgs::from_arg_matches(leaf).map(DoltArtifactCommand::List)
        } else {
            ArtifactShowArgs::from_arg_matches(leaf).map(DoltArtifactCommand::Show)
        }
        .map_err(|error| invalid(safe_parse_message(error.kind())))?;
        let target = match &command {
            DoltArtifactCommand::List(args) => &args.target,
            DoltArtifactCommand::Show(args) => &args.target,
        };
        target
            .validate_common(
                &common,
                "Dolt Artifact reads use an explicit local target and --scope.",
            )
            .map_err(invalid)?;
        Ok(Invocation {
            common,
            command: Command::DoltArtifact(command),
            command_name: command_name.clone(),
        })
    })())
}

fn recover_unknown_long(
    cli: &clap::Command,
    args: &[OsString],
    partial: &ArgMatches,
    error: &clap::Error,
    domain: &str,
) -> Option<ArgMatches> {
    if error.kind() != ErrorKind::UnknownArgument {
        return None;
    }
    let (action @ ("list" | "show"), _) = partial.subcommand_matches(domain)?.subcommand()? else {
        return None;
    };
    let Some(ContextValue::String(invalid)) = error.get(ContextKind::InvalidArg) else {
        return None;
    };
    let long = invalid.strip_prefix("--")?.split('=').next()?;
    if !long.starts_with(|ch: char| ch.is_ascii_alphanumeric())
        || !long
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    let leaf = cli.find_subcommand(domain)?.find_subcommand(action)?;
    if leaf.get_arguments().any(|arg| {
        arg.get_long() == Some(long)
            || arg
                .get_all_aliases()
                .is_some_and(|aliases| aliases.contains(&long))
    }) {
        return None;
    }

    // Let Clap replay the original tokens. Label list has no positionals, so
    // its temporary option can also take one plain separated value. The
    // original parse error still determines the result.
    let allow_separated_value = domain == "label" && action == "list";
    const UNKNOWN_LONG: &str = "recovered_unknown_long";
    let retry = cli.clone().mut_subcommand(domain, |command| {
        command.mut_subcommand(action, |leaf| {
            leaf.arg(
                Arg::new(UNKNOWN_LONG)
                    .long(long.to_owned())
                    .num_args(0..=1)
                    .require_equals(!allow_separated_value)
                    .value_parser(OsStringValueParser::new())
                    .hide(true),
            )
        })
    });
    let matches = retry.try_get_matches_from(args).ok()?;
    let recovered = matches
        .subcommand_matches(domain)?
        .subcommand_matches(action)?;
    (recovered.value_source(UNKNOWN_LONG) == Some(ValueSource::CommandLine)).then_some(matches)
}

struct ExtendedParse {
    cli: clap::Command,
    parsed: Result<ArgMatches, clap::Error>,
    partial: Option<ArgMatches>,
}

fn resource_parser(cli: &clap::Command, args: &[OsString]) -> Option<ExtendedParse> {
    if !args.iter().any(|arg| arg == "resource") {
        return None;
    }
    // Only select this extension after Clap has recognized the Resource domain
    // and contract 2. Contract 1 uses the original command tree and errors.
    let extended = cli
        .clone()
        .subcommand(add_leaf_options(ResourceCommand::augment_subcommands(
            clap::Command::new("resource")
                .subcommand_required(true)
                .hide(true),
        )));
    let parsed = extended.clone().try_get_matches_from(args);
    let partial = if parsed.is_err() {
        let recovery =
            extended_error_options(extended.clone()).mut_subcommand("resource", |domain| {
                // List options on show still have known token boundaries.
                let list_options = domain
                    .find_subcommand("list")
                    .expect("Resource list command")
                    .get_arguments()
                    .filter(|arg| {
                        matches!(
                            arg.get_id().as_str(),
                            "all" | "area" | "issue" | "limit" | "cursor"
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                extended_help_probe(domain)
                    .mut_subcommand("list", extended_error_options)
                    .mut_subcommand("show", |show| {
                        extended_error_options(show.args(list_options))
                    })
            });
        recovery.ignore_errors(true).try_get_matches_from(args).ok()
    } else {
        None
    };
    let matches = parsed.as_ref().ok().or(partial.as_ref())?;
    let domain = matches.subcommand_matches("resource")?;
    let leaf = domain.subcommand().map(|(_, leaf)| leaf);
    (requested_contract(Some(matches), leaf) == Contract::V2).then_some(ExtendedParse {
        cli: extended,
        parsed,
        partial,
    })
}

fn label_parser(cli: &clap::Command, args: &[OsString]) -> Option<ExtendedParse> {
    if !args.iter().any(|arg| arg == "label") {
        return None;
    }
    let extended = defer_project_check(cli.clone()).subcommand(
        add_leaf_options(LabelCommand::augment_subcommands(
            clap::Command::new("label")
                .subcommand_required(true)
                .hide(true),
        ))
        .mut_subcommand("list", |list| {
            defer_project_check(list).mut_arg("project", |arg| {
                arg.help("Select Labels on direct member Issues of this Project UUIDv7.")
            })
        }),
    );
    let parsed = extended.clone().try_get_matches_from(args);
    let partial = parsed.as_ref().err().and_then(|error| {
        let recovery = extended_error_options(extended.clone()).mut_subcommand("label", |label| {
            extended_help_probe(label).mut_subcommand("list", extended_error_options)
        });
        let partial = recovery
            .clone()
            .ignore_errors(true)
            .try_get_matches_from(args)
            .ok()?;
        Some(recover_unknown_long(&recovery, args, &partial, error, "label").unwrap_or(partial))
    });
    let matches = parsed.as_ref().ok().or(partial.as_ref())?;
    let domain = matches.subcommand_matches("label")?;
    let leaf = domain.subcommand().map(|(_, leaf)| leaf);
    (requested_contract(Some(matches), leaf) == Contract::V2).then_some(ExtendedParse {
        cli: extended,
        parsed,
        partial,
    })
}

fn extended_error_options(command: clap::Command) -> clap::Command {
    // Recover only output selection. Keep Clap's token boundaries, including
    // attached literal values and everything after the end-of-options marker.
    extended_help_probe(command)
        .args_override_self(true)
        .mut_args(|arg| {
            let arg = match arg.get_id().as_str() {
                "json" => {
                    // A bare switch has an empty occurrence. Keep attached
                    // values inert and retain all independent occurrences.
                    return arg
                        .action(ArgAction::Append)
                        .overrides_with(clap::builder::Resettable::Reset)
                        .value_parser(OsStringValueParser::new())
                        .num_args(0..=1)
                        .require_equals(true);
                }
                "at" | "project" => arg.value_parser(StringValueParser::new()),
                "format" => arg.value_parser(StringValueParser::new().map(|value| {
                    if value == "json" {
                        Format::Json
                    } else {
                        Format::Text
                    }
                })),
                _ => arg,
            };
            if arg.is_positional() || arg.get_id() == "extended_help_probe" {
                arg
            } else if arg.get_action().takes_values() {
                arg.allow_negative_numbers(true).num_args(0..=1)
            } else {
                // Recover from a malformed attached flag value without
                // consuming a subsequent token.
                arg.action(ArgAction::Set)
                    .value_parser(StringValueParser::new())
                    .num_args(0..=1)
                    .require_equals(true)
            }
        })
}

fn extended_help_probe(command: clap::Command) -> clap::Command {
    command.disable_help_flag(true).arg(
        Arg::new("extended_help_probe")
            .short('h')
            .long("help")
            .action(ArgAction::SetTrue),
    )
}

fn retry_issue_query(cli: &clap::Command, args: &[OsString]) -> Option<ArgMatches> {
    // A malformed Project must reach the v2 UUID check after Clap has read
    // the contract and format. Keep the original parser for all other commands.
    let retry = defer_project_check(cli.clone().mut_subcommand("issue", |issue| {
        issue
            .mut_subcommand("list", defer_project_check)
            .mut_subcommand("ready", defer_project_check)
            .mut_subcommand("blocked", defer_project_check)
    }));
    let matches = retry.try_get_matches_from(args).ok()?;
    let (command, leaf) = matched_leaf(cli, &matches)?;
    (matches!(
        command.as_str(),
        "issue list" | "issue ready" | "issue blocked"
    ) && requested_contract(Some(&matches), Some(leaf)) == Contract::V2)
        .then_some(matches)
}

fn retry_config_show(
    cli: &clap::Command,
    args: &[OsString],
    error_matches: &mut Option<ArgMatches>,
) -> Option<Result<ArgMatches, clap::Error>> {
    // Add v2-only options on retry to preserve legacy config help and errors.
    let accept_numeric_project = |command: clap::Command| {
        defer_project_check(command).mut_arg("project", |arg| arg.allow_negative_numbers(true))
    };
    let retry = accept_numeric_project(cli.clone().mut_subcommand("config", |config| {
        config.mut_subcommand("show", |show| {
            accept_numeric_project(show)
                .arg(Arg::new("area").long("area").allow_negative_numbers(true))
                .arg(
                    Arg::new("resource")
                        .long("resource")
                        .value_parser(value_parser!(PathBuf)),
                )
        })
    }));
    let is_v2_config = |matches: &ArgMatches| {
        matched_leaf(cli, matches).is_some_and(|(command, leaf)| {
            command == "config show"
                && requested_contract(Some(matches), Some(leaf)) == Contract::V2
        })
    };
    match retry.clone().try_get_matches_from(args) {
        Ok(matches) => is_v2_config(&matches).then_some(Ok(matches)),
        Err(error) => {
            // Recover metadata past duplicate, missing, or empty values.
            // Clap still distinguishes flags from values.
            let relaxed = |arg: Arg| {
                arg.action(ArgAction::Append)
                    .num_args(0..=1)
                    .value_parser(OsStringValueParser::new())
            };
            *error_matches = retry
                .mut_subcommand("config", |config| {
                    config.mut_subcommand("show", |show| {
                        show.mut_arg("area", relaxed).mut_arg("resource", relaxed)
                    })
                })
                .try_get_matches_from(args)
                .ok()
                .filter(is_v2_config);
            error_matches.as_ref().map(|_| Err(error))
        }
    }
}

fn retry_scoped_read_failure(
    cli: &clap::Command,
    args: &[OsString],
    error: &clap::Error,
    domain: &str,
) -> Option<ArgMatches> {
    if !matches!(
        error.kind(),
        ErrorKind::ValueValidation
            | ErrorKind::InvalidValue
            | ErrorKind::UnknownArgument
            | ErrorKind::ArgumentConflict
            | ErrorKind::TooManyValues
    ) {
        return None;
    }
    // Recover the output envelope from Clap's actual option boundaries. The
    // original parse error still applies and these values never reach dispatch.
    let recover_value = |arg: clap::Arg| {
        arg.action(ArgAction::Append)
            .allow_negative_numbers(true)
            .num_args(0..=1)
    };
    let recover_scope = |command| {
        defer_project_check(command).mut_args(|arg| match arg.get_id().as_str() {
            "project" => recover_value(arg),
            "at" | "workspace" | "project_root" => {
                recover_value(arg.value_parser(StringValueParser::new()))
            }
            "authority" | "workspace_scope" if domain == "directive" => {
                recover_value(arg.value_parser(StringValueParser::new())).require_equals(true)
            }
            _ => arg,
        })
    };
    let retry = recover_scope(cli.clone().mut_subcommand(domain, |command| {
        command.mut_subcommands(|leaf| {
            recover_scope(leaf).mut_args(|arg| match arg.get_id().as_str() {
                "database" | "workspace_id" | "port" | "cursor" | "area" => recover_value(arg),
                "status" | "limit" | "after" => {
                    recover_value(arg.value_parser(StringValueParser::new()))
                }
                "bot_id" | "id" => arg
                    .value_parser(StringValueParser::new())
                    .allow_negative_numbers(true),
                _ => arg,
            })
        })
    }));
    let matches = retry.clone().try_get_matches_from(args).ok().or_else(|| {
        if domain != "directive" || error.kind() != ErrorKind::UnknownArgument {
            return None;
        }
        let partial = retry
            .clone()
            .ignore_errors(true)
            .try_get_matches_from(args)
            .ok()?;
        // Recovery only needs output metadata, even if show has no ID.
        let retry = retry.mut_subcommand(domain, |command| {
            command.mut_subcommand("show", |show| show.mut_arg("id", |id| id.required(false)))
        });
        recover_unknown_long(&retry, args, &partial, error, domain)
    })?;
    let (command, leaf) = matched_leaf(cli, &matches)?;
    (command
        .split_once(' ')
        .is_some_and(|(matched, _)| matched == domain)
        && requested_contract(Some(&matches), Some(leaf)) == Contract::V2)
        .then_some(matches)
}

fn retry_conversation_failure(
    cli: &clap::Command,
    args: &[OsString],
    kind: ErrorKind,
    action: &str,
) -> Option<ArgMatches> {
    if !matches!(
        kind,
        ErrorKind::UnknownArgument
            | ErrorKind::ArgumentConflict
            | ErrorKind::ValueValidation
            | ErrorKind::InvalidValue
            | ErrorKind::InvalidUtf8
            | ErrorKind::TooManyValues
    ) {
        return None;
    }
    const SCOPED_VALUES: &[&str] = &["workspace", "project", "project-root", "at"];
    let leaf_values: &[&str] = if action == "list" {
        &["bot", "limit", "cursor", "workspace-id", "database", "port"]
    } else {
        &["bot", "id", "workspace-id", "database", "port"]
    };
    let recover_metadata = |command: clap::Command, leaf: bool| {
        command.mut_args(|arg| {
            let Some(long) = arg.get_long() else {
                return arg;
            };
            if !SCOPED_VALUES.contains(&long)
                && !(leaf && leaf_values.contains(&long))
                && long != "authority"
            {
                return arg;
            }
            let attached_only = long == "authority";
            arg.action(ArgAction::Append)
                .value_parser(OsStringValueParser::new())
                .num_args(0..=1)
                .require_equals(attached_only)
        })
    };
    let retry = recover_metadata(cli.clone(), false)
        .mut_subcommand("conversation", |conversation| {
            conversation.mut_subcommand(action, |leaf| recover_metadata(leaf, true))
        });
    // Join a split single-dash value only after a known value-taking option.
    // Clap still recognizes all long options, equals values and the -- boundary.
    // These matches supply output metadata only. The original error is retained.
    let mut args = args.iter().cloned().peekable();
    let mut normalized = Vec::new();
    normalized.extend(args.next());
    while let Some(mut arg) = args.next() {
        if arg == "--" {
            normalized.push(arg);
            normalized.extend(args);
            break;
        }
        if arg
            .to_str()
            .and_then(|arg| arg.strip_prefix("--"))
            .is_some_and(|name| SCOPED_VALUES.contains(&name) || leaf_values.contains(&name))
            && args.peek().is_some_and(
                |value| matches!(value.as_encoded_bytes(), [b'-', next, ..] if *next != b'-'),
            )
        {
            arg.push("=");
            arg.push(args.next().expect("peeked selector value"));
        }
        normalized.push(arg);
    }
    let matches = retry.try_get_matches_from(normalized).ok()?;
    let (command, leaf) = matched_leaf(cli, &matches)?;
    (command == format!("conversation {action}")
        && requested_contract(Some(&matches), Some(leaf)) == Contract::V2)
        .then_some(matches)
}

fn retry_area_read(cli: &clap::Command, args: &[OsString]) -> Option<ArgMatches> {
    // Recover the output options for the original error. A forbidden Project
    // or negative cursor can stop the initial parse before it sees them.
    let retry = defer_project_check(cli.clone().mut_subcommand("area", |area| {
        area.mut_subcommand("list", |list| {
            defer_project_check(list).mut_arg("cursor", |arg| arg.allow_negative_numbers(true))
        })
        .mut_subcommand("tree", defer_project_check)
        .mut_subcommand("show", defer_project_check)
    }));
    let matches = retry.try_get_matches_from(args).ok()?;
    let (command, leaf) = matched_leaf(cli, &matches)?;
    (matches!(command.as_str(), "area list" | "area tree" | "area show")
        && requested_contract(Some(&matches), Some(leaf)) == Contract::V2)
        .then_some(matches)
}

fn defer_project_check(command: clap::Command) -> clap::Command {
    command.mut_arg("project", |arg| {
        arg.value_parser(StringValueParser::new().map(ProjectId::Ordinary))
    })
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
                .try_get_occurrences::<OsString>("json")
                .ok()
                .flatten()
                .is_some_and(|mut occurrences| {
                    occurrences.any(|mut values| values.next().is_none())
                })
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
                Self::Area(command) => command.validate_dolt_selection(common),
                Self::Resource(command) => command.validate_dolt_selection(common),
                Self::Label(LabelCommand::List(args)) => {
                    if !args.target.has_all()
                        || common.workspace.is_some()
                        || common.project_root.is_some()
                        || !matches!(common.view, View::WorkingTree)
                    {
                        return Err(
                            "Dolt Label reads require --workspace-id, --database, and --port with explicit scope.",
                        );
                    }
                    Ok(())
                }
                Self::Issue(IssueCommand::Show(args)) => args.validate_dolt_selection(common),
                Self::Conversation(ConversationCommand::Show(args)) => {
                    if common.workspace.is_none() || !args.target.has_all() {
                        return Err(
                            "Conversation show requires --workspace, --workspace-id, --database, and --port.",
                        );
                    }
                    if common.project.is_some()
                        || common.project_root.is_some()
                        || !matches!(common.view, View::WorkingTree)
                    {
                        return Err(
                            "Conversation show accepts only explicit workspace and conversation selection.",
                        );
                    }
                    Ok(())
                }
                Self::Issue(
                    IssueCommand::List(args)
                    | IssueCommand::Ready(args)
                    | IssueCommand::Blocked(args),
                ) => args.validate_dolt_selection(common),
                Self::Project(_) => Err("Use contract 2 Project read options."),
                Self::Bot(command) => command.validate(common),
                Self::Directive(command) => command.validate(common),
                Self::Artifact(ArtifactCommand::List { .. } | ArtifactCommand::Show { .. }) => {
                    Err("Use Dolt Artifact read options with --contract 2.")
                }
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
            Self::Area(_) => Err("Area commands require --contract 2."),
            Self::Issue(_) => Err("Issue commands require --contract 2."),
            Self::Conversation(_) => Err("Conversation commands require --contract 2."),
            Self::Bot(command) => command.validate(common),
            Self::Directive(command) => command.validate(common),
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

impl DirectiveCommand {
    fn validate(&self, common: &CommonOptions) -> Result<(), &'static str> {
        let selection = match self {
            Self::List { selection, .. } | Self::Show { selection, .. } => selection,
        };
        let cursor = matches!(
            self,
            Self::List {
                cursor: Some(_),
                ..
            }
        );
        if common.contract == Contract::V1 {
            if selection.target.has_any()
                || selection.workspace_scope
                || selection.area.is_some()
                || cursor
            {
                return Err("Dolt Directive options require --contract 2.");
            }
            return Ok(());
        }
        if common.workspace.is_some()
            || common.project_root.is_some()
            || !matches!(common.view, View::WorkingTree)
        {
            return Err(
                "Dolt Directive reads require an explicit local database and workspace ID.",
            );
        }
        if !selection.target.has_all() {
            return Err("Dolt Directive reads require --workspace-id, --database, and --port.");
        }
        if matches!(
            self,
            Self::List {
                page: ListArgs { after: Some(_), .. },
                ..
            }
        ) {
            return Err("Use --cursor for Dolt Directive pagination.");
        }
        Ok(())
    }
}

impl BotCommand {
    fn validate(&self, common: &CommonOptions) -> Result<(), &'static str> {
        let target = match self {
            Self::List { target, .. } | Self::Show { target, .. } => target,
        };
        if common.contract == Contract::V1 {
            if target.has_any()
                || matches!(
                    self,
                    Self::List {
                        status: Some(_),
                        ..
                    } | Self::List {
                        cursor: Some(_),
                        ..
                    } | Self::Show {
                        asset_commit: Some(_),
                        ..
                    }
                )
            {
                return Err("Dolt Bot options require --contract 2.");
            }
            return Ok(());
        }
        if matches!(
            self,
            Self::Show {
                asset_commit: Some(_),
                ..
            }
        ) {
            if common.workspace.is_none() {
                return Err("Soul inspection requires an explicit --workspace.");
            }
            if common.project.is_some()
                || common.project_root.is_some()
                || !matches!(common.view, View::WorkingTree)
            {
                return Err("Soul inspection accepts only explicit workspace and Bot selection.");
            }
        } else {
            target.validate_common(
                common,
                "Dolt Bot inspection accepts only an explicit local database and workspace ID.",
            )?;
        }
        if !target.has_all() {
            return Err("Dolt Bot inspection requires --workspace-id, --database, and --port.");
        }
        if matches!(
            self,
            Self::List {
                page: ListArgs { after: Some(_), .. },
                ..
            }
        ) {
            return Err("Use --cursor for Dolt Bot pagination.");
        }
        Ok(())
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

impl AreaCommand {
    fn validate_dolt_selection(&self, common: &CommonOptions) -> Result<(), &'static str> {
        let target = match self {
            Self::List(args) => &args.target,
            Self::Tree(args) => &args.target,
            Self::Show(args) => &args.target,
        };
        target.validate_common(
            common,
            "Dolt Area reads accept only an explicit local database and workspace ID.",
        )?;
        if !target.has_all() {
            return Err("Dolt Area reads require --workspace-id, --database, and --port.");
        }
        Ok(())
    }
}

impl ResourceCommand {
    fn validate_dolt_selection(&self, common: &CommonOptions) -> Result<(), &'static str> {
        let target = match self {
            Self::List(args) => &args.target,
            Self::Show(args) => &args.target,
        };
        target.validate_common(
            common,
            "Dolt Resource reads accept only an explicit local database and workspace ID.",
        )?;
        if !target.has_all() {
            return Err("Dolt Resource reads require --workspace-id, --database, and --port.");
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

impl IssueListArgs {
    fn validate_dolt_selection(&self, common: &CommonOptions) -> Result<(), &'static str> {
        if !self.target.has_all() {
            return Err("Dolt issue list requires --workspace-id, --database, and --port.");
        }
        if common.workspace.is_some()
            || common.project_root.is_some()
            || !matches!(common.view, View::WorkingTree)
        {
            return Err("Dolt issue list requires explicit Dolt selection and scope.");
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
