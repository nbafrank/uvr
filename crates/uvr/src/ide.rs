//! IDE detection and mode resolution.
//!
//! uvr writes IDE-specific configuration (Positron's `.vscode/settings.json`)
//! and prints IDE-oriented hints. That behavior is opt-in: it is enabled when
//! uvr can see Positron in the environment (`POSITRON=1`, set by its
//! integrated terminal) or when the user forces it with `--ide`. The default
//! is no IDE, so CI and plain terminals stay clean.
//!
//! `--ide` is the extension point for other editors: add a variant to [`Ide`],
//! a detection branch, and the config writer when there is config worth
//! writing. RStudio, for example, needs no config here — its library wiring is
//! already covered by `.Rprofile`.
//!
//! `.Rprofile` is deliberately *not* part of this: it is the library wiring
//! any R session started from the project root needs, IDE or not.

use clap::ValueEnum;

/// The IDE a uvr invocation should generate config for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Ide {
    /// No IDE — don't write IDE config or print IDE hints. Not a valid `--ide`
    /// spelling; use `--no-ide`.
    #[value(skip)]
    None,
    /// Positron — write `.vscode/settings.json` (`positron.r.*`, `r.rterm`,
    /// `r.rpath`).
    Positron,
}

impl Ide {
    /// Detect an IDE from the environment variable set by its integrated
    /// terminal. Positron sets `POSITRON=1`.
    pub fn detect() -> Self {
        if env_is_set("POSITRON") {
            Ide::Positron
        } else {
            Ide::None
        }
    }

    /// Combine explicit CLI overrides with environment detection.
    ///
    /// Precedence: `--ide` > (`--no-ide` | `UVR_UNATTENDED=1`)
    /// > detected `POSITRON=1` > `None`.
    pub fn resolve(cli: Option<Ide>, no_ide: bool) -> Self {
        if let Some(ide) = cli {
            return ide;
        }
        if no_ide || uvr_core::env_vars::unattended() {
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
    fn detect_positron() {
        let _env = env_lock();
        with_env(&[("POSITRON", Some("1"))], || {
            assert_eq!(Ide::detect(), Ide::Positron);
        });
    }

    #[test]
    fn detect_falls_back_to_none() {
        let _env = env_lock();
        with_env(&[("POSITRON", None)], || {
            assert_eq!(Ide::detect(), Ide::None);
        });
    }

    #[test]
    fn resolve_order_is_cli_then_unattended_then_detected() {
        let _env = env_lock();
        // Explicit --ide wins over everything.
        with_env(
            &[("POSITRON", Some("1")), ("UVR_UNATTENDED", Some("1"))],
            || {
                assert_eq!(Ide::resolve(Some(Ide::Positron), false), Ide::Positron);
            },
        );

        // --no-ide / UVR_UNATTENDED force None.
        with_env(&[("POSITRON", Some("1")), ("UVR_UNATTENDED", None)], || {
            assert_eq!(Ide::resolve(None, true), Ide::None);
        });
        with_env(
            &[("POSITRON", Some("1")), ("UVR_UNATTENDED", Some("1"))],
            || {
                assert_eq!(Ide::resolve(None, false), Ide::None);
            },
        );

        // No override: env detection wins. Clear UVR_UNATTENDED so the result
        // doesn't depend on the terminal the suite is run from.
        with_env(&[("POSITRON", Some("1")), ("UVR_UNATTENDED", None)], || {
            assert_eq!(Ide::resolve(None, false), Ide::Positron);
        });
    }
}
