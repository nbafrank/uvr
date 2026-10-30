use anyhow::Result;

use uvr_core::project::Project;
use uvr_core::r_version::detector::{find_r_binary_satisfying, find_r_binary_with_source};

use crate::ui::palette;

/// `uvr r find [constraint]` — print the path to an R binary, and nothing
/// else on stdout, so `$(uvr r find)` works in scripts.
///
/// With a constraint, prints the newest working R that satisfies it. Pins and
/// `UVR_R_VERSION` are ignored here: they outrank the constraint in
/// `find_r_binary`, so honouring them could print an R that does not satisfy
/// the constraint the user asked for.
///
/// Without a constraint, prints the R the current project uses, resolved
/// like `uvr run` and `uvr doctor` do: `UVR_R_VERSION`, the `.r-version` pin,
/// the global pin, the uvr.toml constraint, then the newest managed R, then
/// system R. The rule that selected it goes to stderr.
pub fn run(constraint: Option<String>) -> Result<()> {
    let (binary, source) = match constraint {
        Some(c) => (find_r_binary_satisfying(&c)?, None),
        None => {
            let project_constraint = Project::find_cwd()
                .ok()
                .and_then(|p| p.manifest.project.r_version);
            let (binary, source) = find_r_binary_with_source(project_constraint.as_deref())?;
            (binary, Some(source))
        }
    };
    println!("{}", binary.display());
    if let Some(source) = source {
        eprintln!("{}", palette::dim(format!("selected by: {source}")));
    }
    Ok(())
}
