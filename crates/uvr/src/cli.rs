use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

#[derive(Debug, Parser)]
#[command(
    name = "uvr",
    version,
    about = "Fast, reproducible R package management",
    long_about = None,
)]
pub struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Suppress all output except errors
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Create a new uvr project in the current directory
    Init(InitArgs),

    /// Add one or more packages to the project
    Add(AddArgs),

    /// Remove one or more packages from the project
    Remove(RemoveArgs),

    /// Install all packages from the lockfile
    Sync(SyncArgs),

    /// Run an R script within the project environment
    Run(RunArgs),

    /// Update packages to their latest allowed versions
    Update(UpdateArgs),

    /// Update the lockfile without installing
    Lock(LockArgs),

    /// Show the dependency tree
    Tree(TreeArgs),

    /// Export lockfile to other formats (e.g. renv.lock)
    Export(ExportArgs),

    /// Generate shell completions
    Completions(CompletionsArgs),

    /// Update uvr itself to the latest release
    SelfUpdate,

    /// Manage R versions
    #[command(name = "r")]
    R(RArgs),

    /// Diagnose environment issues
    Doctor,

    /// Manage the local download cache
    #[command(name = "cache")]
    Cache(CacheArgs),
}

// ────────────────────────────────────────────────────────────
//  init
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Project name (defaults to current directory name)
    pub name: Option<String>,

    /// R version constraint, e.g. ">=4.3.0"
    #[arg(long = "r-version", value_name = "CONSTRAINT")]
    pub r_version: Option<String>,
}

// ────────────────────────────────────────────────────────────
//  add
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Packages to add, e.g. `ggplot2`, `ggplot2@>=3.0.0`, `user/repo@ref`
    #[arg(required = true, value_name = "PKG[@VERSION|user/repo@REF]")]
    pub packages: Vec<String>,

    /// Add as dev dependency
    #[arg(long)]
    pub dev: bool,

    /// Package comes from Bioconductor
    #[arg(long)]
    pub bioc: bool,

    /// Number of parallel download jobs
    #[arg(short, long, default_value = "4", value_name = "N")]
    pub jobs: usize,
}

// ────────────────────────────────────────────────────────────
//  remove
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Package names to remove
    #[arg(required = true)]
    pub packages: Vec<String>,
}

// ────────────────────────────────────────────────────────────
//  sync
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Fail if the lockfile is out of date (CI mode)
    #[arg(long)]
    pub frozen: bool,

    /// Number of parallel download jobs
    #[arg(short, long, default_value = "4", value_name = "N")]
    pub jobs: usize,
}

// ────────────────────────────────────────────────────────────
//  run
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Override the R version for this run, e.g. "4.3.2" or ">=4.2.0"
    #[arg(long = "r-version", value_name = "VERSION")]
    pub r_version: Option<String>,

    /// Extra packages to make available for this run (cached, not added to manifest)
    #[arg(long = "with", value_name = "PKG")]
    pub with_packages: Vec<String>,

    /// R script to execute
    pub script: Option<String>,

    /// Arguments forwarded to the script
    #[arg(last = true)]
    pub args: Vec<String>,
}

// ────────────────────────────────────────────────────────────
//  lock
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct LockArgs {
    /// Re-resolve and upgrade all packages to their latest allowed versions
    #[arg(long)]
    pub upgrade: bool,
}

// ────────────────────────────────────────────────────────────
//  update
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Specific packages to update (updates all if omitted)
    pub packages: Vec<String>,

    /// Show what would change without installing
    #[arg(long)]
    pub dry_run: bool,

    /// Number of parallel download jobs
    #[arg(short, long, default_value = "4", value_name = "N")]
    pub jobs: usize,
}

// ────────────────────────────────────────────────────────────
//  tree
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct TreeArgs {
    /// Maximum display depth
    #[arg(long, value_name = "N")]
    pub depth: Option<usize>,
}

// ────────────────────────────────────────────────────────────
//  export
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Output format (currently: renv)
    #[arg(long, default_value = "renv")]
    pub format: String,

    /// Output file path (prints to stdout if omitted)
    #[arg(short, long)]
    pub output: Option<String>,
}

// ────────────────────────────────────────────────────────────
//  completions
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    pub shell: Shell,
}

// ────────────────────────────────────────────────────────────
//  r (subcommand group)
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct RArgs {
    #[command(subcommand)]
    pub command: RCommands,
}

#[derive(Debug, Subcommand)]
pub enum RCommands {
    /// Download and install a specific R version
    Install(RInstallArgs),

    /// List installed R versions
    List(RListArgs),

    /// Set the R version constraint in uvr.toml and write .r-version
    Use(RUseArgs),

    /// Write an exact R version to .r-version (like uv python pin)
    Pin(RPinArgs),
}

#[derive(Debug, Args)]
pub struct RInstallArgs {
    /// R version to install, e.g. "4.3.2"
    pub version: String,
}

#[derive(Debug, Args)]
pub struct RListArgs {
    /// Show all available versions (requires network)
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct RUseArgs {
    /// R version constraint to set in uvr.toml, e.g. ">=4.3.0"
    pub version: String,
}

#[derive(Debug, Args)]
pub struct RPinArgs {
    /// Exact R version to pin in .r-version (e.g. "4.3.2").
    /// If omitted, uses the currently active R version.
    pub version: Option<String>,
}

// ────────────────────────────────────────────────────────────
//  cache
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub command: CacheCommands,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommands {
    /// Remove all cached package downloads
    Clean,
}
