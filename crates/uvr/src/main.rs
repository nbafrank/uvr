mod cli;
mod commands;

use anyhow::Result;
use clap::Parser;
use console::style;
use tracing_subscriber::{fmt, EnvFilter};

use cli::{CacheCommands, Cli, Commands, RCommands};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        // Forward exact exit code from `uvr run` script failures.
        if let Some(script_err) = e.downcast_ref::<commands::run::ScriptExitError>() {
            std::process::exit(script_err.0);
        }
        eprintln!("{} {e:#}", style("error:").red().bold());
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

    match cli.command {
        Commands::Init(args) => {
            commands::init::run(args.name, args.r_version)?;
        }
        Commands::Add(args) => {
            commands::add::run(args.packages, args.dev, args.bioc, args.jobs).await?;
        }
        Commands::Remove(args) => {
            commands::remove::run(args.packages).await?;
        }
        Commands::Sync(args) => {
            commands::sync::run(args.frozen, args.jobs).await?;
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
            let format: commands::export::ExportFormat = args
                .format
                .parse()
                .map_err(|e: String| anyhow::anyhow!(e))?;
            commands::export::run(format, args.output)?;
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
            RCommands::Install(args) => {
                commands::r_cmd::install::run(args.version).await?;
            }
            RCommands::List(args) => {
                commands::r_cmd::list::run(args.all).await?;
            }
            RCommands::Use(args) => {
                commands::r_cmd::use_version::run(args.version)?;
            }
            RCommands::Pin(args) => {
                commands::r_cmd::pin::run(args.version)?;
            }
        },
        Commands::Cache(cache_args) => match cache_args.command {
            CacheCommands::Clean => {
                commands::cache::run_clean()?;
            }
        },
    }

    Ok(())
}
