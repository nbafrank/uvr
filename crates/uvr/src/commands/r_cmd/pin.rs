use anyhow::{bail, Context, Result};

use uvr_core::project::{remove_global_r_version_pin, write_global_r_version_pin, Project};
use uvr_core::r_version::detector::{
    find_all, find_r_binary, is_plausible_r_version, pin_conflicts_with_constraint,
    query_r_version, version_matches_prefix,
};

use crate::ui;
use crate::ui::palette;

/// `uvr r pin [version]` — write an exact version to `.r-version`.
///
/// If no version is given, queries the currently active R binary.
///
/// With `global`, writes the per-user default R instead, which needs no
/// project (#329). `unset` removes that default.
pub fn run(version: Option<String>, global: bool, unset: bool) -> Result<()> {
    if unset {
        return unset_global();
    }
    if global {
        return pin_global(version);
    }

    let project = Project::find_cwd().context("Not inside a uvr project")?;

    let pinned = match version {
        Some(v) => {
            check_requested_version(&v)?;
            v
        }
        None => {
            let constraint = project.manifest.project.r_version.as_deref();
            let binary = find_r_binary(constraint)
                .context("R not found. Install R or use `uvr r install <version>`")?;
            let resolved = query_r_version(&binary)
                .context("Could not determine R version from the active R binary")?;
            // find_r_binary honours an existing `.r-version` pin over the
            // manifest constraint, so the resolved version can still violate
            // [project] r_version (e.g. a stale pin at 4.3 in a `^4.5`
            // project). Refuse to re-write such a pin: silently persisting a
            // version that overrides the manifest on every later resolution
            // is how projects drift (#137).
            ensure_pin_satisfies_constraint(&resolved, constraint)?;
            resolved
        }
    };

    project
        .write_r_version_pin(&pinned)
        .context("Failed to write .r-version")?;

    ui::success(format!(
        "Pinned R {} {} {}",
        palette::info(&pinned),
        palette::dim(ui::glyph::arrow()),
        palette::dim(".r-version"),
    ));

    Ok(())
}

fn pin_global(version: Option<String>) -> Result<()> {
    let pinned = match version {
        Some(v) => {
            check_requested_version(&v)?;
            v
        }
        None => {
            let constraint = Project::find_cwd()
                .ok()
                .and_then(|p| p.manifest.project.r_version);
            let binary = find_r_binary(constraint.as_deref())
                .context("R not found. Install R or use `uvr r install <version>`")?;
            query_r_version(&binary)
                .context("Could not determine R version from the active R binary")?
        }
    };

    let path = write_global_r_version_pin(&pinned).context("Failed to write the global R pin")?;

    ui::success(format!(
        "Pinned R {} {} {}",
        palette::info(&pinned),
        palette::dim(ui::glyph::arrow()),
        palette::dim(path.display().to_string()),
    ));

    Ok(())
}

fn unset_global() -> Result<()> {
    match remove_global_r_version_pin().context("Failed to remove the global R pin")? {
        Some(path) => ui::success(format!(
            "Removed the global R pin {} {}",
            palette::dim(ui::glyph::arrow()),
            palette::dim(path.display().to_string()),
        )),
        None => ui::info("No global R pin is set"),
    }
    Ok(())
}

/// Validate before writing: the pin used to accept any string, and garbage
/// (`--`, `4-5-2`) produced a `.r-version` that could never match an install
/// (#171).
fn check_requested_version(v: &str) -> Result<()> {
    if !is_plausible_r_version(v) {
        bail!(
            "`{v}` is not a valid R version to pin. Expected `X.Y.Z` (e.g. 4.5.1) \
             or a partial `X.Y` (e.g. 4.5)."
        );
    }
    let installed = find_all();
    if !installed
        .iter()
        .any(|i| i.version == v || version_matches_prefix(v, &i.version))
    {
        ui::warn(format!(
            "R {v} is not installed yet — run `uvr r install {v}` to use this pin."
        ));
    }
    Ok(())
}

/// Refuse an implicit (no-arg) pin when the resolved R version violates the
/// manifest's `[project] r_version` constraint (#137). Explicit
/// `uvr r pin <version>` stays allowed — that's a deliberate override.
fn ensure_pin_satisfies_constraint(resolved: &str, constraint: Option<&str>) -> Result<()> {
    if let Some(constraint) = constraint {
        if pin_conflicts_with_constraint(resolved, constraint) {
            bail!(
                "The active R is {resolved}, which does not satisfy the [project] \
                 r_version constraint `{constraint}` in uvr.toml. Pin a matching \
                 version with `uvr r pin <version>`, or update the constraint."
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_pin_refuses_constraint_violation() {
        // #137: a no-arg pin must not persist a version the manifest forbids.
        let err = ensure_pin_satisfies_constraint("3.6.3", Some("^4.5")).unwrap_err();
        assert!(err.to_string().contains("^4.5"), "{err}");
        assert!(err.to_string().contains("3.6.3"), "{err}");
    }

    #[test]
    fn implicit_pin_allows_satisfying_version() {
        ensure_pin_satisfies_constraint("4.5.1", Some("^4.5")).unwrap();
        ensure_pin_satisfies_constraint("4.5.1", Some(">=4.4")).unwrap();
    }

    #[test]
    fn implicit_pin_allows_unconstrained_projects() {
        ensure_pin_satisfies_constraint("3.6.3", None).unwrap();
    }

    #[test]
    fn implicit_pin_ignores_unparseable_constraints() {
        // Mirrors find_r_binary's drift warning: constraints the resolver
        // itself couldn't act on must not block the pin here.
        ensure_pin_satisfies_constraint("4.5.1", Some("not-a-constraint")).unwrap();
    }
}
