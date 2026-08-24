//! IDE detection and mode resolution.
//!
//! uvr writes IDE-specific configuration (Positron's `.vscode/settings.json`)
//! and prints IDE-oriented hints. That behavior is opt-in: it is enabled when
//! uvr can see an IDE in the environment (`POSITRON=1` / `RSTUDIO=1`, both set
//! by the respective IDE's integrated terminal) or when the user forces it
//! with `--ide`. The default is no IDE, so CI and plain terminals stay clean.
//!
//! `.Rprofile` is deliberately *not* part of this: it is the library wiring
//! any R session started from the project root needs, IDE or not.

use clap::ValueEnum;

/// The IDE a uvr invocation should generate config for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ide {
    /// No IDE — don't write IDE config or print IDE hints.
    None,
    /// Positron — write `.vscode/settings.json` (`positron.r.*`, `r.rterm`,
    /// `r.rpath`).
    Positron,
    /// RStudio — no `.vscode` config today; `.Rprofile` is already written
    /// for every mode.
    Rstudio,
}

impl Ide {
    /// Detect an IDE from the environment variables set by its integrated
    /// terminal. Positron sets `POSITRON=1`; RStudio sets `RSTUDIO=1`.
    pub fn detect() -> Self {
        if env_is_set("POSITRON") {
            Ide::Positron
        } else if env_is_set("RSTUDIO") {
            Ide::Rstudio
        } else {
            Ide::None
        }
    }

    /// Combine explicit CLI overrides with environment detection.
    ///
    /// Precedence: `--ide` > (`--no-ide` | `--unattended` | `UVR_UNATTENDED=1`)
    /// > detected `POSITRON=1` / `RSTUDIO=1` > `None`.
    pub fn resolve(cli: Option<Ide>, no_ide: bool, unattended: bool) -> Self {
        if let Some(ide) = cli {
            return ide;
        }
        if no_ide || unattended || uvr_core::env_vars::unattended() {
            return Ide::None;
        }
        Self::detect()
    }

    /// Whether to write the Positron/VSCode `.vscode/settings.json` and the
    /// IDE-oriented "no `.r-version` pin" hint.
    pub fn is_positron(self) -> bool {
        self == Ide::Positron
    }
}

/// The `--ide` CLI value. Kept separate from [`Ide`] so `--ide=none` is not
/// a valid spelling — use `--no-ide` for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IdeArg {
    Positron,
    Rstudio,
}

impl IdeArg {
    pub fn into_ide(self) -> Ide {
        match self {
            IdeArg::Positron => Ide::Positron,
            IdeArg::Rstudio => Ide::Rstudio,
        }
    }
}

fn env_is_set(name: &str) -> bool {
    std::env::var_os(name)
        .map(|v| !v.to_string_lossy().trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes every test that mutates process-global environment
    /// variables. Env vars are shared across the whole test binary, so
    /// `with_env` blocks would otherwise race under the parallel runner.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn with_env<F: FnOnce()>(vars: &[(&'static str, Option<&'static str>)], f: F) {
        let mut backups = Vec::new();
        for &(name, val) in vars {
            backups.push((name, std::env::var_os(name)));
            match val {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        f();
        for (name, prev) in backups {
            match prev {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn detect_prefers_positron_over_rstudio() {
        let _env = env_lock();
        with_env(&[("POSITRON", Some("1")), ("RSTUDIO", Some("1"))], || {
            assert_eq!(Ide::detect(), Ide::Positron);
        });
    }

    #[test]
    fn detect_falls_back_to_none() {
        let _env = env_lock();
        with_env(&[("POSITRON", None), ("RSTUDIO", None)], || {
            assert_eq!(Ide::detect(), Ide::None);
        });
    }

    #[test]
    fn resolve_order_is_cli_then_unattended_then_detected() {
        let _env = env_lock();
        // Explicit --ide wins over everything.
        with_env(&[("POSITRON", Some("1")), ("UVR_UNATTENDED", Some("1"))], || {
            assert_eq!(
                Ide::resolve(Some(Ide::Rstudio), false, false),
                Ide::Rstudio
            );
        });

        // --no-ide / --unattended / UVR_UNATTENDED all force None.
        with_env(&[("POSITRON", Some("1"))], || {
            assert_eq!(Ide::resolve(None, true, false), Ide::None);
            assert_eq!(Ide::resolve(None, false, true), Ide::None);
        });
        with_env(&[("POSITRON", Some("1")), ("UVR_UNATTENDED", Some("1"))], || {
            assert_eq!(Ide::resolve(None, false, false), Ide::None);
        });

        // No override: env detection wins.
        with_env(&[("RSTUDIO", Some("1"))], || {
            assert_eq!(Ide::resolve(None, false, false), Ide::Rstudio);
        });
    }
}
