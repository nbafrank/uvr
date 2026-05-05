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

    // SIGINT / Ctrl+C handler (#58): kill any in-flight `R CMD INSTALL`,
    // remove its 00LOCK-<pkg>/ dir, then exit 130 so the shell sees a
    // standard interrupt code. Without this, the child keeps running and
    // the next sync trips over a stale lock dir.
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            uvr_core::signal::kill_and_cleanup_all();
            // 128 + SIGINT(2) = 130, the shell convention for Ctrl+C exit.
            std::process::exit(130);
        }
    });

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

    // #63/#64 phase 1: warn loudly if the project pin doesn't match the active R.
    // Only for library-affecting commands — `init`, `r ...`, `cache`, etc. don't
    // touch the library and the warning would be noise there.
    if matches!(
        command,
        Commands::Add(_)
            | Commands::Remove(_)
            | Commands::Sync(_)
            | Commands::Run(_)
            | Commands::Update(_)
            | Commands::Lock(_)
            | Commands::Tree(_)
            | Commands::Export(_)
            | Commands::Import(_)
            | Commands::Doctor
    ) {
        commands::util::warn_r_pin_mismatch();
    }

    match command {
        Commands::Init(args) => {
            commands::init::run(args.name, args.here, args.r_version)?;
        }
        Commands::Add(args) => {
            let timeout = parse_cli_timeout(args.timeout.as_deref())?;
            commands::add::run(
                args.packages,
                args.dev,
                args.bioc,
                args.source,
                args.jobs,
                timeout,
                args.no_lock,
                args.no_install,
            )
            .await?;
        }
        Commands::Remove(args) => {
            commands::remove::run(args.packages).await?;
        }
        Commands::Sync(args) => {
            let timeout = parse_cli_timeout(args.timeout.as_deref())?;
            commands::sync::run(args.frozen, args.no_dev, args.jobs, args.library, timeout).await?;
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
            // #71: --input/-i is an alternative spelling of the positional path.
            // clap's `conflicts_with` already rejects passing both.
            let path = args.input.or(args.path);
            commands::import::run(path, args.name, args.lock, args.jobs).await?;
        }
        Commands::Scan(args) => {
            commands::scan::run(args.all)?;
        }
        Commands::Completions(args) => {
            commands::completions::run(args.shell)?;
        }
        Commands::Upgrade(args) => {
            commands::self_update::run(args.check).await?;
        }
        Commands::Doctor => {
            commands::doctor::run()?;
        }
        Commands::R(r_args) => match r_args.command {
            Some(RCommands::Install(args)) => {
                commands::r_cmd::install::run(args.version, args.distribution).await?;
            }
            Some(RCommands::Uninstall(args)) => {
                commands::r_cmd::uninstall::run(args.version)?;
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
            Some(RCommands::Javareconf) => {
                commands::r_cmd::javareconf::run()?;
            }
            None => {
                ui::welcome_group(
                    "r",
                    "Manage R versions",
                    &[
                        ("uvr r install <ver>", "Download and install an R version"),
                        ("uvr r uninstall <ver>", "Remove an uvr-managed R version"),
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

    let full = match &context {
        Some(c) => format!("{headline}\n{c}"),
        None => headline.clone(),
    };
    let hint = hint_for(&full);
    ui::error_block(&headline, context.as_deref(), hint);
}

/// Parse the `--timeout <DURATION>` CLI flag value into a `Duration`.
/// Empty / `None` → `Ok(None)` (caller falls back to env var or default).
fn parse_cli_timeout(s: Option<&str>) -> Result<Option<std::time::Duration>> {
    let Some(raw) = s.map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    uvr_core::installer::r_cmd_install::parse_install_timeout(raw)
        .map(Some)
        .ok_or_else(|| {
            anyhow::anyhow!("Invalid --timeout value: {raw} (expected e.g. 30m, 2h, 90s)")
        })
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
    } else if m.contains("emutls_w") || m.contains("/opt/gfortran") {
        Some(
            "Missing Fortran toolchain on macOS. Install the CRAN gfortran build from \
             https://mac.r-project.org/tools/ or run `brew install gcc` — required for \
             source packages with Fortran (e.g. edgeR, limma).",
        )
    } else if m.contains("unable to locate a java runtime")
        || m.contains("java interpreter")
        || m.contains("no java runtime present")
    {
        Some(
            "Missing Java runtime. Install a JDK (e.g. `brew install --cask temurin`), \
             then run `uvr r javareconf` to register the JVM with your project's \
             managed R. Required for rJava and packages that depend on it (e.g. xlsx, RJDBC).",
        )
    } else {
        None
    }
}
