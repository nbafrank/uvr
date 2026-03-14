mod cli;
mod commands;

use anyhow::Result;
use clap::Parser;
use console::style;
use tracing_subscriber::{fmt, EnvFilter};

use cli::{Cli, Commands, RCommands};

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
            commands::remove::run(args.packages)?;
        }
        Commands::Sync(args) => {
            commands::sync::run(args.frozen, args.jobs).await?;
        }
        Commands::Run(args) => {
            commands::run::run(args.script, args.args)?;
        }
        Commands::Lock(args) => {
            commands::lock::run(args.upgrade).await?;
        }
        Commands::R(r_args) => match r_args.command {
            RCommands::Install(args) => {
                commands::r_cmd::install::run(args.version).await?;
            }
            RCommands::List(args) => {
                commands::r_cmd::list::run(args.all)?;
            }
            RCommands::Use(args) => {
                commands::r_cmd::use_version::run(args.version)?;
            }
            RCommands::Pin(args) => {
                commands::r_cmd::pin::run(args.version)?;
            }
        },
    }

    Ok(())
}
