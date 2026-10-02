use anyhow::Result;

use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary, find_r_binary_ignoring_pin};

/// `uvr r find [constraint]` — print the path to an R binary, and nothing
/// else, so `$(uvr r find)` works in scripts.
///
/// With a constraint, prints the newest working R that satisfies it. A
/// `.r-version` pin is ignored here: the pin outranks the constraint in
/// `find_r_binary`, so honouring it could print an R that does not satisfy
/// the constraint the user asked for.
///
/// Without a constraint, prints the R the current project uses, resolved
/// like `uvr run` and `uvr doctor` do: the `.r-version` pin, then the
/// uvr.toml constraint, then the newest managed R, then system R.
pub fn run(constraint: Option<String>) -> Result<()> {
    let binary = match constraint {
        Some(c) => find_r_binary_ignoring_pin(Some(&c))?,
        None => {
            let project_constraint = Project::find_cwd()
                .ok()
                .and_then(|p| p.manifest.project.r_version);
            find_r_binary(project_constraint.as_deref())?
        }
    };
    println!("{}", binary.display());
    Ok(())
}
