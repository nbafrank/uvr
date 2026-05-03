use std::path::PathBuf;

use clap::builder::styling;
use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

use crate::commands::export::ExportFormat;

/// Match the runtime palette: cyan accents for headers/usage, magenta for
/// literal flag names, yellow for placeholders. Keeps `--help` visually of
/// a piece with the rest of uvr's output. Clap 4 made help styling opt-in,
/// so without this block the help text renders flat.
const HELP_STYLES: styling::Styles = styling::Styles::styled()
    .header(styling::AnsiColor::Cyan.on_default().bold())
    .usage(styling::AnsiColor::Cyan.on_default().bold())
    .literal(styling::AnsiColor::Magenta.on_default().bold())
    .placeholder(styling::AnsiColor::Yellow.on_default());

#[derive(Debug, Parser)]
#[command(
    name = "uvr",
    version,
    about = "Fast, reproducible R package management",
    long_about = None,
    styles = HELP_STYLES,
)]
pub struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Suppress all output except errors
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
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

    /// Import packages from an renv.lock file
    Import(ImportArgs),

    /// Generate shell completions
    Completions(CompletionsArgs),

    /// Check for and install the latest uvr release
    #[command(aliases = ["self-update"])]
    Upgrade(UpgradeArgs),

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
    /// Project name (positional). When provided, a new directory of this
    /// name is created and the project is initialized inside it (matches
    /// `uv init <name>` semantics from #56). Pass `--here` to keep the old
    /// behavior of initializing in the current directory.
    pub name: Option<String>,

    /// Initialize in the current directory instead of creating a new one.
    /// When combined with a positional name, the project name is set in
    /// uvr.toml without creating a directory (preserves pre-0.3 behavior).
    #[arg(long)]
    pub here: bool,

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

    /// Custom repository URL (CRAN-like), e.g. https://community.r-multiverse.org
    #[arg(long, value_name = "URL")]
    pub source: Option<String>,

    /// Number of parallel download jobs
    #[arg(short, long, default_value = "50", value_name = "N")]
    pub jobs: usize,

    /// Per-package install timeout, e.g. `30m`, `2h`, `90s`. Defaults to 30m
    /// (override via `UVR_INSTALL_TIMEOUT`).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,

    /// Update uvr.toml only — skip lockfile resolution and install.
    /// Useful when scripting multiple `uvr add` calls and running
    /// `uvr lock` + `uvr sync` once at the end (#76).
    #[arg(long)]
    pub no_lock: bool,

    /// Update uvr.toml and lockfile, but skip install. Run `uvr sync`
    /// to install later. (Note: `--no-lock` already implies this since
    /// there's no fresh lockfile to install from.) (#76)
    #[arg(long)]
    pub no_install: bool,
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

    /// Skip dev-only packages (production deploy)
    #[arg(long)]
    pub no_dev: bool,

    /// Number of parallel download jobs
    #[arg(short, long, default_value = "50", value_name = "N")]
    pub jobs: usize,

    /// Install packages to a custom library path instead of .uvr/library/
    /// (also reads UVR_LIBRARY env var)
    #[arg(long, value_name = "PATH")]
    pub library: Option<PathBuf>,

    /// Per-package install timeout, e.g. `30m`, `2h`, `90s`. Defaults to 30m
    /// (override via `UVR_INSTALL_TIMEOUT`).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
}

// ────────────────────────────────────────────────────────────
//  run
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Override the R version for this run, e.g. "4.3.2" or ">=4.2.0"
    #[arg(long = "r-version", value_name = "VERSION")]
    pub r_version: Option<String>,

    /// Extra packages to make available for this run (cached in ~/.uvr/cache/with-envs/,
    /// not added to manifest; persists until `uvr cache clean`)
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
    #[arg(short, long, default_value = "50", value_name = "N")]
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
    #[arg(long, value_enum, default_value = "renv")]
    pub format: ExportFormat,

    /// Output file path (prints to stdout if omitted)
    #[arg(short, long)]
    pub output: Option<String>,
}

// ────────────────────────────────────────────────────────────
//  import
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Path to renv.lock file (defaults to ./renv.lock).
    /// Equivalent to `--input` / `-i`; mirrors `uvr export -o <FILE>` (#71).
    pub path: Option<String>,

    /// Alternative way to spell the input path — for symmetry with
    /// `uvr export --output <FILE>` (#71). Mutually exclusive with the
    /// positional `path` argument.
    #[arg(long, short = 'i', value_name = "FILE", conflicts_with = "path")]
    pub input: Option<String>,

    /// Override the project name written to `uvr.toml`. Defaults to the
    /// current directory's basename (or, in merge mode, preserves the
    /// existing project name unless `--name` is given) (#77).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Resolve and install packages after import
    #[arg(long)]
    pub lock: bool,

    /// Number of parallel download jobs
    #[arg(short, long, default_value = "50", value_name = "N")]
    pub jobs: usize,
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
    pub command: Option<RCommands>,
}

#[derive(Debug, Subcommand)]
pub enum RCommands {
    /// Download and install a specific R version
    Install(RInstallArgs),

    /// Remove a uvr-managed R installation
    Uninstall(RUninstallArgs),

    /// List installed R versions
    List(RListArgs),

    /// Set the R version constraint in uvr.toml and write .r-version
    Use(RUseArgs),

    /// Write an exact R version to .r-version (like uv python pin)
    Pin(RPinArgs),

    /// Run `sudo R CMD javareconf` against the project's managed R to register the JVM
    Javareconf,
}

#[derive(Debug, Args)]
pub struct RInstallArgs {
    /// R version to install, e.g. "4.3.2"
    pub version: String,

    /// Override the autodetected Linux distribution for the Posit CDN URL
    /// (e.g. `ubuntu-2204`, `debian-12`, `rhel-9`). Useful on Ubuntu / Debian
    /// derivatives that aren't matched by `/etc/os-release` autodetection
    /// — PopOS, Manjaro, etc. (#54). Ignored on macOS / Windows.
    #[arg(long, value_name = "SLUG")]
    pub distribution: Option<String>,
}

#[derive(Debug, Args)]
pub struct RUninstallArgs {
    /// R version to remove, e.g. "4.3.2"
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
//  upgrade
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// Print the latest version and whether an update is available, without
    /// downloading or installing anything.
    #[arg(long)]
    pub check: bool,
}

// ────────────────────────────────────────────────────────────
//  cache
// ────────────────────────────────────────────────────────────

#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub command: Option<CacheCommands>,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommands {
    /// Remove all cached package downloads
    Clean,
}
