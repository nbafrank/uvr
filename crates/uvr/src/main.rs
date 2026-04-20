mod cli;
mod commands;
mod ui;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{fmt, EnvFilter};

use cli::{CacheCommands, Cli, Commands, RCommands};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        // Forward exact exit code from `uvr run` script failures.
        if let Some(script_err) = e.downcast_ref::<commands::run::ScriptExitError>() {
            std::process::exit(script_err.0);
        }
        render_error(&e);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    // Respect NO_COLOR env var (https://no-color.org/)
    if std::env::var_os("NO_COLOR").is_some() {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }

    let cli = Cli::parse();

    // Initialize tracing
    let filter = if cli.verbose {
        "uvr=debug,uvr_core=debug"
    } else if cli.quiet {
        "error"
    } else {
        "uvr=info,uvr_core=info"
    };
    fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_target(false)
        .without_time()
        .init();

    let Some(command) = cli.command else {
        ui::welcome(env!("CARGO_PKG_VERSION"));
        return Ok(());
    };

    match command {
        Commands::Init(args) => {
            commands::init::run(args.name, args.r_version)?;
        }
        Commands::Add(args) => {
            commands::add::run(args.packages, args.dev, args.bioc, args.source, args.jobs).await?;
        }
        Commands::Remove(args) => {
            commands::remove::run(args.packages).await?;
        }
        Commands::Sync(args) => {
            commands::sync::run(args.frozen, args.no_dev, args.jobs, args.library).await?;
        }
        Commands::Run(args) => {
            commands::run::run(args.script, args.r_version, args.with_packages, args.args).await?;
        }
        Commands::Update(args) => {
            commands::update::run(args.packages, args.dry_run, args.jobs).await?;
        }
        Commands::Lock(args) => {
            commands::lock::run(args.upgrade).await?;
        }
        Commands::Tree(args) => {
            commands::tree::run(args.depth)?;
        }
        Commands::Export(args) => {
            commands::export::run(args.format, args.output)?;
        }
        Commands::Import(args) => {
            commands::import::run(args.path, args.lock, args.jobs).await?;
        }
        Commands::Completions(args) => {
            commands::completions::run(args.shell)?;
        }
        Commands::SelfUpdate => {
            commands::self_update::run().await?;
        }
        Commands::Doctor => {
            commands::doctor::run()?;
        }
        Commands::R(r_args) => match r_args.command {
            Some(RCommands::Install(args)) => {
                commands::r_cmd::install::run(args.version).await?;
            }
            Some(RCommands::List(args)) => {
                commands::r_cmd::list::run(args.all).await?;
            }
            Some(RCommands::Use(args)) => {
                commands::r_cmd::use_version::run(args.version)?;
            }
            Some(RCommands::Pin(args)) => {
                commands::r_cmd::pin::run(args.version)?;
            }
            None => {
                ui::welcome_group(
                    "r",
                    "Manage R versions",
                    &[
                        ("uvr r install <ver>", "Download and install an R version"),
                        ("uvr r list", "List installed R versions"),
                        ("uvr r use <ver>", "Set the R version constraint"),
                        ("uvr r pin <ver>", "Write an exact R version to .r-version"),
                    ],
                );
            }
        },
        Commands::Cache(cache_args) => match cache_args.command {
            Some(CacheCommands::Clean) => {
                commands::cache::run_clean()?;
            }
            None => {
                ui::welcome_group(
                    "cache",
                    "Manage the local download cache",
                    &[("uvr cache clean", "Remove all cached package downloads")],
                );
            }
        },
    }

    Ok(())
}

/// Render an error using the three-part format: headline / context / hint.
///
/// Pulls the top-level error message as the headline, joins the rest of the
/// chain (via `anyhow`'s context chain) as context lines, and picks a hint
/// based on heuristics from the message.
fn render_error(e: &anyhow::Error) {
    let headline = e.to_string();
    let mut context_lines: Vec<String> = Vec::new();
    for cause in e.chain().skip(1) {
        context_lines.push(cause.to_string());
    }
    let context = if context_lines.is_empty() {
        None
    } else {
        Some(context_lines.join("\n"))
    };

    let hint = hint_for(&headline);
    ui::error_block(&headline, context.as_deref(), hint);
}

fn hint_for(msg: &str) -> Option<&'static str> {
    let m = msg.to_ascii_lowercase();
    if m.contains("not inside a uvr project") {
        Some("Run `uvr init` to create uvr.toml in this directory.")
    } else if m.contains("no lockfile") {
        Some("Run `uvr lock` to generate uvr.lock.")
    } else if m.contains("lockfile is out of date") {
        Some("Run `uvr lock` then commit uvr.lock alongside uvr.toml.")
    } else if m.contains("r not found") || m.contains("cannot select r binary") {
        Some("Run `uvr r install <version>` or ensure R is on PATH.")
    } else if m.contains("base r package") {
        Some("Remove this package from your manifest — base packages ship with R.")
    } else {
        None
    }
}
