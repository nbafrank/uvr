use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};

use uvr_core::project::Project;

use crate::ui;
use crate::ui::palette;

pub fn run(depth: Option<usize>) -> Result<()> {
    let project = Project::find_cwd().context("Not inside a uvr project")?;
    let lockfile = project
        .load_lockfile()
        .context("Failed to read uvr.lock")?
        .ok_or_else(|| anyhow::anyhow!("No lockfile found. Run `uvr lock` first."))?;

    if lockfile.packages.is_empty() {
        println!("No packages in lockfile.");
        return Ok(());
    }

    // Build lookup: name → (version, deps)
    let pkg_map: HashMap<&str, (&str, &[String])> = lockfile
        .packages
        .iter()
        .map(|p| (p.name.as_str(), (p.version.as_str(), p.requires.as_slice())))
        .collect();

    // Find root packages: those listed in the manifest (direct deps).
    let direct_deps: HashSet<&str> = project
        .manifest
        .dependencies
        .keys()
        .chain(project.manifest.dev_dependencies.keys())
        .map(|s| s.as_str())
        .collect();

    // Roots are direct deps that exist in the lockfile.
    let mut roots: Vec<&str> = direct_deps
        .iter()
        .filter(|name| pkg_map.contains_key(*name))
        .copied()
        .collect();
    roots.sort();

    let max_depth = depth.unwrap_or(usize::MAX);

    println!(
        "{} {} {}",
        palette::bold(&project.manifest.project.name),
        palette::dim(ui::glyph::bullet()),
        palette::dim(format!("R {}", &lockfile.r.version)),
    );
    println!();

    let mut ctx = TreeCtx {
        pkg_map: &pkg_map,
        max_depth,
        ancestors: HashSet::new(),
    };

    for (i, root) in roots.iter().enumerate() {
        let is_last = i == roots.len() - 1;
        let dev = project.manifest.dev_dependencies.contains_key(*root);
        print_node(root, &mut ctx, "", is_last, dev, 0);
    }

    // Footer: direct / transitive / total.
    let indirect = lockfile.packages.len() - roots.len();
    let sep = format!(" {} ", ui::glyph::bullet());
    println!();
    println!(
        "{}",
        palette::dim(format!(
            "{} direct{sep}{} transitive{sep}{} total",
            roots.len(),
            indirect,
            lockfile.packages.len(),
        ))
    );

    Ok(())
}

struct TreeCtx<'a> {
    pkg_map: &'a HashMap<&'a str, (&'a str, &'a [String])>,
    max_depth: usize,
    /// Tracks packages on the current path from root to this node.
    /// A package is only a cycle if it appears in its own ancestor chain,
    /// NOT just because it was visited in a different branch (diamond deps).
    ancestors: HashSet<String>,
}

fn print_node(
    name: &str,
    ctx: &mut TreeCtx<'_>,
    prefix: &str,
    is_last: bool,
    is_dev: bool,
    depth: usize,
) {
    let connector = if is_last {
        ui::glyph::tree_last()
    } else {
        ui::glyph::tree_branch()
    };

    let (version, deps) = ctx.pkg_map.get(name).copied().unwrap_or(("?", &[]));

    let dev_tag = if is_dev {
        format!(" {}", palette::dim("[dev]"))
    } else {
        String::new()
    };

    // A true cycle: this package is an ancestor of itself in the current path.
    let is_cycle = ctx.ancestors.contains(name);
    let cycle_tag = if is_cycle {
        format!(" {}", palette::warn("(cycle)"))
    } else {
        String::new()
    };

    println!(
        "{}{}{} {}{dev_tag}{cycle_tag}",
        palette::dim(prefix),
        palette::dim(connector),
        palette::pkg(name),
        palette::version(version),
    );

    if is_cycle || depth >= ctx.max_depth {
        return;
    }

    ctx.ancestors.insert(name.to_string());

    let child_prefix = format!(
        "{prefix}{}",
        if is_last {
            ui::glyph::tree_space()
        } else {
            ui::glyph::tree_vert()
        }
    );

    let deps = deps.to_vec();
    for (j, dep) in deps.iter().enumerate() {
        let child_last = j == deps.len() - 1;
        print_node(dep, ctx, &child_prefix, child_last, false, depth + 1);
    }

    ctx.ancestors.remove(name);
}
