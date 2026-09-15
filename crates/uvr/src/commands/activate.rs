//! `uvr activate` — put the project's isolated R environment into the
//! current shell, so a bare `R` or `Rscript` uses the project without an
//! `uvr run` prefix.
//!
//! # Why a shim that calls back into uvr
//!
//! Python's `.venv/bin/activate` can bake absolute paths into a file because
//! the venv *is* the interpreter — it cannot change underneath the script. A
//! uvr environment is computed from `uvr.toml` and `.r-version`, which the
//! user edits (`uvr r use`, `uvr r pin`). A generated script with paths baked
//! in would keep pointing at the old R after such an edit.
//!
//! So `.uvr/activate` holds no paths at all. It asks the binary to recompute
//! the environment every time it is sourced, which makes staleness
//! structurally impossible rather than a thing to remember.

use std::path::Path;

use anyhow::{Context, Result};
use clap::ValueEnum;

use uvr_core::project::{Project, DOT_UVR_DIR};
use uvr_core::r_env::{REnv, PATH_SEP};
use uvr_core::r_version::detector::find_r_binary;

use crate::ui;

/// Shells `uvr activate --emit` can generate code for.
///
/// Deliberately not the same set as `uvr completions`: `sh` is here because a
/// POSIX shim has to name a shell, and `elvish` is not, because nobody has
/// asked for an activation script for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ActivateShell {
    Sh,
    Bash,
    Zsh,
    Fish,
    Powershell,
}

/// Name of the POSIX shim inside `.uvr/`.
pub const SHIM_SH: &str = "activate";
/// Name of the fish shim inside `.uvr/`.
pub const SHIM_FISH: &str = "activate.fish";
/// Name of the PowerShell shim inside `.uvr/`.
///
/// Lowercase on purpose: Windows filenames are case-insensitive, so
/// `. .uvr/activate.ps1` works either way, and a single `.uvr/activate*`
/// gitignore entry then covers all three shims.
pub const SHIM_PS1: &str = "activate.ps1";

/// Environment variables activation takes over, and therefore must save and
/// restore. `PATH` is handled specially (it is prepended to, not replaced);
/// the rest are set to literal values from [`REnv::vars`]. `UVR_PROJECT` is
/// uvr's own marker and is simply unset on deactivate.
fn managed_vars(env: &REnv) -> Vec<&'static str> {
    let mut names = vec!["PATH"];
    names.extend(env.vars().iter().map(|(k, _)| *k));
    names.push("UVR_PROJECT");
    names
}

pub fn run(emit: Option<ActivateShell>, write_shim: bool) -> Result<()> {
    if write_shim {
        let project = find_project()?;
        write_shims(&project.root)?;
        ui::success("Wrote activation shims");
        for name in [SHIM_SH, SHIM_FISH, SHIM_PS1] {
            ui::bullet_dim(format!("{DOT_UVR_DIR}/{name}"));
        }
        return Ok(());
    }

    if let Some(shell) = emit {
        let (name, env, prompt) = resolve()?;
        print!("{}", emit_script(shell, &name, &env, prompt));
        return Ok(());
    }

    // No flags: tell a human what to type. Emitting shell code here would be
    // hostile — the user would get a wall of exports printed to the terminal.
    let project = find_project()?;
    if shims_incomplete(&project.root) {
        write_shims(&project.root)?;
    }
    // `ui::info` renders a one-line headline; the command and the follow-up go
    // through bullet/hint rather than being crammed into it as embedded
    // newlines, which would leave every line but the first unglyphed.
    ui::info("Activate this project in your shell:");
    ui::bullet(format!("source {DOT_UVR_DIR}/{SHIM_SH}"));
    ui::hint("Run `deactivate` to restore your shell.");
    Ok(())
}

/// True when any shim is missing from `<root>/.uvr/`.
///
/// Checks all three rather than just the POSIX one: a project initialized by
/// an older uvr has `.uvr/activate` alone, and testing only that would leave
/// those users with no fish or PowerShell shim to source.
fn shims_incomplete(root: &Path) -> bool {
    [SHIM_SH, SHIM_FISH, SHIM_PS1]
        .iter()
        .any(|name| !root.join(DOT_UVR_DIR).join(name).exists())
}

fn find_project() -> Result<Project> {
    // `.context` rather than `map_err`, matching every other command: it keeps
    // the underlying UvrError in the chain instead of discarding it.
    Project::find_cwd().context("Not inside a uvr project")
}

/// Resolve the project name and its isolated environment.
///
/// Deliberately re-resolves R on every call: that is what keeps a sourced
/// shim correct after `uvr r use` / `uvr r pin`.
fn resolve() -> Result<(String, REnv, bool)> {
    let project = find_project()?;
    project
        .ensure_library_dir()
        .context("Failed to create .uvr/library/")?;

    // Env var wins over the manifest, so a user can opt out of a project
    // that opts in (and vice versa) without editing a shared file.
    let prompt = uvr_core::env_vars::activate_prompt()
        .or_else(|| project.manifest.activate.as_ref().and_then(|a| a.prompt))
        .unwrap_or(false);

    // Matches `uvr run`: the constraint comes from uvr.toml, and
    // `find_r_binary` itself honours a `.r-version` pin walked up from cwd.
    let r_binary = find_r_binary(project.manifest.project.r_version.as_deref())
        .context("R not found. Install R or use `uvr r install <version>`")?;

    let site_profile = uvr_core::r_version::openmp::runtime_site_profile_for_binary(&r_binary);
    let env = REnv {
        r_binary,
        library: project.library_path(),
        with_library: None,
        extra_libs: uvr_core::env_vars::extra_libs(),
        site_profile,
    };
    Ok((project.manifest.project.name.clone(), env, prompt))
}

fn emit_script(shell: ActivateShell, project: &str, env: &REnv, prompt: bool) -> String {
    match shell {
        ActivateShell::Sh | ActivateShell::Bash | ActivateShell::Zsh => {
            emit_posix(project, env, prompt)
        }
        ActivateShell::Fish => emit_fish(project, env, prompt),
        ActivateShell::Powershell => emit_powershell(project, env, prompt),
    }
}

/// Single-quote a value for POSIX shells, escaping embedded single quotes.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Single-quote for fish, which honours `\\` and `\'` inside single quotes.
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

/// Single-quote for PowerShell, where an embedded quote is doubled.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn emit_posix(project: &str, env: &REnv, prompt: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# uvr activation for project {project} — generated by `uvr activate --emit sh`.\n"
    ));

    // Unwind a previous *uvr* activation first, so re-activating is
    // idempotent rather than stacking a second R bin dir onto PATH. Gated on
    // UVR_PROJECT, not on `deactivate` merely existing: Python's virtualenv
    // uses that same function name, and an unguarded call would silently
    // deactivate a venv the user was working in.
    out.push_str(
        "if [ -n \"${UVR_PROJECT-}\" ] && command -v deactivate >/dev/null 2>&1; \
         then deactivate; fi\n",
    );

    // PS1 is a plain variable in POSIX shells, so it rides the same
    // save/restore machinery as everything else.
    let mut saved = managed_vars(env);
    if prompt {
        saved.push("PS1");
    }

    // Save prior state. The `${VAR+1}` marker distinguishes "was set to
    // empty" from "was never set", so deactivate can restore either exactly.
    //
    // Deliberately NOT exported: `deactivate` runs in this same shell, so
    // these only need to be shell variables. Exporting them would push a
    // dozen-plus UVR_OLD_* entries into every child process, R included.
    for name in &saved {
        out.push_str(&format!(
            "UVR_OLD_{name}=\"${{{name}-}}\"; UVR_OLD_{name}_SET=\"${{{name}+1}}\"\n"
        ));
    }

    // Apply. PATH is prepended so the project's R wins over any system R.
    out.push_str(&format!(
        "PATH={}\"{PATH_SEP}${{PATH-}}\"; export PATH\n",
        sh_quote(&env.r_bin_dir().to_string_lossy())
    ));
    for (name, value) in env.vars() {
        out.push_str(&format!("{name}={}; export {name}\n", sh_quote(&value)));
    }
    out.push_str(&format!(
        "UVR_PROJECT={}; export UVR_PROJECT\n",
        sh_quote(project)
    ));
    if prompt {
        // Deliberately not exported: PS1 is a shell variable. Exporting it
        // would push the prompt into every child process, and deactivate
        // could not put that back.
        out.push_str(&format!(
            "PS1={}\"${{PS1-}}\"\n",
            sh_quote(&format!("({project}) "))
        ));
    }

    // deactivate: restore every saved variable, drop our bookkeeping, and
    // remove itself so the shell is left exactly as it was found.
    out.push_str("deactivate() {\n");
    for name in &saved {
        // PS1 is restored without `export` for the same reason it is set
        // without it — see above.
        let export = if *name == "PS1" {
            ""
        } else {
            " export {name};"
        };
        let export = export.replace("{name}", name);
        out.push_str(&format!(
            "    if [ -n \"${{UVR_OLD_{name}_SET-}}\" ]; then {name}=\"${{UVR_OLD_{name}-}}\";\
             {export} else unset {name}; fi\n"
        ));
    }
    let bookkeeping: Vec<String> = saved
        .iter()
        .flat_map(|n| [format!("UVR_OLD_{n}"), format!("UVR_OLD_{n}_SET")])
        .collect();
    out.push_str(&format!("    unset {}\n", bookkeeping.join(" ")));
    out.push_str("    unset -f deactivate\n");
    out.push_str("}\n");
    out
}

fn emit_fish(project: &str, env: &REnv, prompt: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# uvr activation for project {project} — generated by `uvr activate --emit fish`.\n"
    ));
    // Gated on UVR_PROJECT so we never deactivate a Python venv, which uses
    // the same function name. See emit_posix.
    out.push_str("if set -q UVR_PROJECT; and functions -q deactivate\n    deactivate\nend\n");

    // fish has no `${VAR+1}`; `set -q` is the set/unset test.
    for name in managed_vars(env) {
        out.push_str(&format!(
            "if set -q {name}\n    set -g UVR_OLD_{name} ${name}\n    set -g UVR_OLD_{name}_SET 1\n\
             else\n    set -g UVR_OLD_{name}_SET ''\nend\n"
        ));
    }

    // PATH is a genuine list in fish, so prepending is a list operation
    // rather than string concatenation with a separator.
    out.push_str(&format!(
        "set -gx PATH {} $PATH\n",
        fish_quote(&env.r_bin_dir().to_string_lossy())
    ));
    for (name, value) in env.vars() {
        out.push_str(&format!("set -gx {name} {}\n", fish_quote(&value)));
    }
    out.push_str(&format!("set -gx UVR_PROJECT {}\n", fish_quote(project)));

    // fish builds its prompt from a `fish_prompt` function, not a variable,
    // so the old one is copied aside and delegated to.
    if prompt {
        out.push_str(&format!(
            "if functions -q fish_prompt\n    functions -c fish_prompt _uvr_old_fish_prompt\nend\n\
             function fish_prompt\n    printf '%s' {}\n    _uvr_old_fish_prompt\nend\n",
            fish_quote(&format!("({project}) "))
        ));
    }

    out.push_str("function deactivate\n");
    for name in managed_vars(env) {
        out.push_str(&format!(
            "    if test -n \"$UVR_OLD_{name}_SET\"\n        set -gx {name} $UVR_OLD_{name}\n\
             \x20   else\n        set -e {name}\n    end\n"
        ));
    }
    let bookkeeping: Vec<String> = managed_vars(env)
        .iter()
        .flat_map(|n| [format!("UVR_OLD_{n}"), format!("UVR_OLD_{n}_SET")])
        .collect();
    out.push_str(&format!("    set -e {}\n", bookkeeping.join(" ")));
    if prompt {
        out.push_str(
            "    if functions -q _uvr_old_fish_prompt\n        functions -e fish_prompt\n\
             \x20       functions -c _uvr_old_fish_prompt fish_prompt\n\
             \x20       functions -e _uvr_old_fish_prompt\n    end\n",
        );
    }
    out.push_str("    functions -e deactivate\nend\n");
    out
}

/// Emit PowerShell activation.
///
/// Assigning `''` through the `$env:` provider sets a present-but-empty
/// variable that child processes inherit — checked on PowerShell 7.6, where
/// R sees `R_LIBS_SITE=` and drops the system site library. That matters
/// because Win32's `SetEnvironmentVariable` *deletes* a variable given an
/// empty string; were PowerShell to route through that, the three blanked
/// isolation variables would go absent and the site library would leak back.
/// `test_activate_powershell_isolation_survives_a_real_shell` pins the
/// behaviour and runs on the Windows CI runner, so that divergence would
/// surface there rather than in a user's session.
fn emit_powershell(project: &str, env: &REnv, prompt: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# uvr activation for project {project} — generated by `uvr activate --emit powershell`.\n"
    ));
    // Gated on UVR_PROJECT so we never deactivate a Python venv, which uses
    // the same function name. See emit_posix.
    out.push_str("if ($env:UVR_PROJECT -and (Test-Path Function:\\deactivate)) { deactivate }\n");

    for name in managed_vars(env) {
        out.push_str(&format!(
            "if (Test-Path Env:\\{name}) {{ $env:UVR_OLD_{name} = $env:{name}; \
             $env:UVR_OLD_{name}_SET = '1' }} else {{ $env:UVR_OLD_{name}_SET = '' }}\n"
        ));
    }

    out.push_str(&format!(
        "$env:PATH = {} + '{PATH_SEP}' + $env:PATH\n",
        ps_quote(&env.r_bin_dir().to_string_lossy())
    ));
    for (name, value) in env.vars() {
        out.push_str(&format!("$env:{name} = {}\n", ps_quote(&value)));
    }
    out.push_str(&format!("$env:UVR_PROJECT = {}\n", ps_quote(project)));

    // PowerShell's prompt is a function too. Capture it once — guarding
    // against a re-activation that would otherwise capture our own wrapper
    // and nest the prefix.
    if prompt {
        out.push_str(&format!(
            "if (-not (Test-Path Variable:Global:_UVR_OLD_PROMPT)) \
             {{ $Global:_UVR_OLD_PROMPT = $function:prompt }}\n\
             function global:prompt {{ {} + (& $Global:_UVR_OLD_PROMPT) }}\n",
            ps_quote(&format!("({project}) "))
        ));
    }

    out.push_str("function global:deactivate {\n");
    for name in managed_vars(env) {
        out.push_str(&format!(
            "    if ($env:UVR_OLD_{name}_SET) {{ $env:{name} = $env:UVR_OLD_{name} }} \
             else {{ Remove-Item Env:\\{name} -ErrorAction SilentlyContinue }}\n"
        ));
    }
    for name in managed_vars(env) {
        out.push_str(&format!(
            "    Remove-Item Env:\\UVR_OLD_{name} -ErrorAction SilentlyContinue\n\
             \x20   Remove-Item Env:\\UVR_OLD_{name}_SET -ErrorAction SilentlyContinue\n"
        ));
    }
    if prompt {
        out.push_str(
            "    if (Test-Path Variable:Global:_UVR_OLD_PROMPT) \
             { Set-Item Function:global:prompt $Global:_UVR_OLD_PROMPT; \
             Remove-Variable -Name _UVR_OLD_PROMPT -Scope Global -ErrorAction SilentlyContinue }\n",
        );
    }
    out.push_str("    Remove-Item Function:\\deactivate -ErrorAction SilentlyContinue\n}\n");
    out
}

/// The sourceable shim. Contains no paths on purpose — see the module docs.
const SHIM_SH_BODY: &str = r#"# uvr project environment — source this file to activate:
#
#     source .uvr/activate
#
# Generated by uvr; do not edit. This file contains no paths: it asks the
# uvr binary to recompute the environment on every activation, so changing
# the project's R version never leaves it stale.
if ! command -v uvr >/dev/null 2>&1; then
    echo "uvr: not found on PATH — cannot activate this project." >&2
else
    # Only eval on success, so a failed resolve (no project, no R) leaves
    # the shell untouched instead of half-activated.
    __uvr_activate_out="$(uvr activate --emit sh)" && eval "$__uvr_activate_out"
    unset __uvr_activate_out
fi
"#;

const SHIM_FISH_BODY: &str = r#"# uvr project environment — source this file to activate:
#
#     source .uvr/activate.fish
#
# Generated by uvr; do not edit. Contains no paths: the environment is
# recomputed by the uvr binary on every activation.
if not command -q uvr
    echo "uvr: not found on PATH — cannot activate this project." >&2
else
    # Only eval on success, so a failed resolve leaves the shell untouched.
    set -l __uvr_activate_out (uvr activate --emit fish | string collect)
    if test $status -eq 0
        eval $__uvr_activate_out
    end
end
"#;

const SHIM_PS1_BODY: &str = r#"# uvr project environment — dot-source this file to activate:
#
#     . .uvr/activate.ps1
#
# Generated by uvr; do not edit. Contains no paths: the environment is
# recomputed by the uvr binary on every activation.
if (-not (Get-Command uvr -ErrorAction SilentlyContinue)) {
    Write-Error "uvr: not found on PATH — cannot activate this project."
} else {
    # Only invoke on success, so a failed resolve leaves the shell untouched.
    $uvrActivateOut = & uvr activate --emit powershell
    if ($LASTEXITCODE -eq 0) {
        Invoke-Expression ($uvrActivateOut -join "`n")
    }
}
"#;

/// Write the activation shims into `<root>/.uvr/`.
///
/// All shells get a shim regardless of the current platform — a project is
/// shared, and the person who cloned it may not use the shell that ran
/// `uvr init`.
pub fn write_shims(root: &Path) -> Result<()> {
    let dir = root.join(DOT_UVR_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    for (name, body) in [
        (SHIM_SH, SHIM_SH_BODY),
        (SHIM_FISH, SHIM_FISH_BODY),
        (SHIM_PS1, SHIM_PS1_BODY),
    ] {
        std::fs::write(dir.join(name), body)
            .with_context(|| format!("Failed to write {DOT_UVR_DIR}/{name}"))?;
    }
    // Generated files must not be committed. Done here rather than only in
    // `uvr init` so `--write-shim` and the backfill path can't produce
    // untracked-but-unignored shims in a project created by an older uvr.
    // Idempotent — it skips entries already present.
    crate::commands::init::write_gitignore(root)
        .context("Failed to update .gitignore for activation shims")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn env() -> REnv {
        REnv {
            r_binary: PathBuf::from("/opt/R/4.4.2/bin/R"),
            library: PathBuf::from("/proj/.uvr/library"),
            with_library: None,
            extra_libs: None,
            site_profile: None,
        }
    }

    #[test]
    fn posix_prepends_the_project_r_to_path() {
        let out = emit_posix("demo", &env(), false);
        assert!(
            out.contains(&format!("PATH='/opt/R/4.4.2/bin'\"{PATH_SEP}${{PATH-}}\"")),
            "PATH not prepended:\n{out}"
        );
    }

    #[test]
    fn posix_exports_every_variable_the_run_command_sets() {
        // Anti-drift: activation must export exactly what `uvr run` applies.
        // If someone adds a variable to REnv::vars, this fails until the
        // emitter carries it too.
        let env = env();
        let out = emit_posix("demo", &env, false);
        for (name, value) in env.vars() {
            assert!(
                out.contains(&format!("{name}={}; export {name}", sh_quote(&value))),
                "{name} missing from emitted script:\n{out}"
            );
        }
    }

    #[test]
    fn posix_defines_a_deactivate_that_removes_itself() {
        let out = emit_posix("demo", &env(), false);
        assert!(out.contains("deactivate() {"));
        assert!(out.contains("unset -f deactivate"));
        // Restores rather than blindly unsetting.
        assert!(out.contains(r#"if [ -n "${UVR_OLD_PATH_SET-}" ]; then PATH="${UVR_OLD_PATH-}""#));
    }

    #[test]
    fn posix_unwinds_a_previous_activation_before_applying() {
        // Without this, re-activating stacks a second R bin dir onto PATH.
        let out = emit_posix("demo", &env(), false);
        let deactivate_guard = out
            .find("&& command -v deactivate")
            .expect("no unwind guard");
        let path_apply = out.find("PATH='/opt/R").expect("no PATH assignment");
        assert!(
            deactivate_guard < path_apply,
            "unwind must come before applying:\n{out}"
        );
    }

    #[test]
    fn no_shell_deactivates_a_python_venv() {
        // Python's virtualenv defines a function called `deactivate` too.
        // Unwinding on its mere existence would silently tear down a venv the
        // user was working in, so every shell gates on UVR_PROJECT first.
        let env = env();
        for (shell, script) in [
            ("posix", emit_posix("demo", &env, false)),
            ("fish", emit_fish("demo", &env, false)),
            ("powershell", emit_powershell("demo", &env, false)),
        ] {
            // Everything before the first mention of `deactivate` is the
            // guard; it must test UVR_PROJECT.
            let first = script
                .find("deactivate")
                .unwrap_or_else(|| panic!("{shell}: no unwind at all"));
            assert!(
                script[..first].contains("UVR_PROJECT"),
                "{shell}: unwind is not gated on UVR_PROJECT, so it would \
                 tear down a Python venv:\n{}",
                &script[..first + 20]
            );
        }
    }

    #[test]
    fn posix_bookkeeping_is_not_pushed_into_child_processes() {
        // deactivate runs in this shell, so UVR_OLD_* need not be exported —
        // exporting would leak a dozen entries into every child, R included.
        let out = emit_posix("demo", &env(), false);
        assert!(
            !out.contains("export UVR_OLD_"),
            "bookkeeping vars are exported:\n{out}"
        );
    }

    #[test]
    fn posix_exports_the_project_name() {
        assert!(emit_posix("my-proj", &env(), false)
            .contains("UVR_PROJECT='my-proj'; export UVR_PROJECT"));
    }

    #[test]
    fn posix_quotes_paths_containing_spaces_and_quotes() {
        let env = REnv {
            library: PathBuf::from("/pro j/it's/.uvr/library"),
            ..env()
        };
        let out = emit_posix("demo", &env, false);
        assert!(
            out.contains(r"'/pro j/it'\''s/.uvr/library'"),
            "path not safely quoted:\n{out}"
        );
    }

    #[test]
    fn shim_delegates_instead_of_baking_in_paths() {
        // The staleness guarantee: no absolute path may appear in the shim.
        assert!(SHIM_SH_BODY.contains("uvr activate --emit sh"));
        assert!(!SHIM_SH_BODY.contains("R_LIBS_USER"));
        assert!(!SHIM_SH_BODY.contains("/bin/R"));
    }

    #[test]
    fn shim_only_evals_when_resolution_succeeds() {
        // A failed resolve must leave the shell untouched, not half-applied.
        assert!(SHIM_SH_BODY.contains(r#"" && eval "#));
    }

    #[test]
    fn write_shims_creates_a_sourceable_file_for_every_shell() {
        let tmp = tempfile::tempdir().unwrap();
        write_shims(tmp.path()).unwrap();
        for (name, expect) in [
            (SHIM_SH, "uvr activate --emit sh"),
            (SHIM_FISH, "uvr activate --emit fish"),
            (SHIM_PS1, "uvr activate --emit powershell"),
        ] {
            let shim = tmp.path().join(DOT_UVR_DIR).join(name);
            assert!(shim.exists(), "{name} not written");
            assert!(
                std::fs::read_to_string(&shim).unwrap().contains(expect),
                "{name} does not delegate to the binary"
            );
        }
    }

    #[test]
    fn fish_prepends_to_the_path_list_and_exports_every_var() {
        let env = env();
        let out = emit_fish("demo", &env, false);
        // fish PATH is a real list — prepend as a list element, not by
        // splicing a separator into a string.
        assert!(
            out.contains("set -gx PATH '/opt/R/4.4.2/bin' $PATH"),
            "{out}"
        );
        for (name, value) in env.vars() {
            assert!(
                out.contains(&format!("set -gx {name} {}", fish_quote(&value))),
                "{name} missing:\n{out}"
            );
        }
        assert!(out.contains("function deactivate"));
        assert!(out.contains("functions -e deactivate"));
        assert!(out.contains("if set -q UVR_PROJECT; and functions -q deactivate"));
    }

    #[test]
    fn powershell_prepends_to_path_and_exports_every_var() {
        let env = env();
        let out = emit_powershell("demo", &env, false);
        assert!(
            out.contains(&format!(
                "$env:PATH = '/opt/R/4.4.2/bin' + '{PATH_SEP}' + $env:PATH"
            )),
            "{out}"
        );
        for (name, value) in env.vars() {
            assert!(
                out.contains(&format!("$env:{name} = {}", ps_quote(&value))),
                "{name} missing:\n{out}"
            );
        }
        assert!(out.contains("function global:deactivate {"));
        assert!(out.contains("Remove-Item Function:\\deactivate"));
        assert!(out.contains("if ($env:UVR_PROJECT -and (Test-Path Function:\\deactivate))"));
    }

    #[test]
    fn every_shell_manages_the_same_variables() {
        // The shells must not drift apart: whatever one takes over, the
        // others must take over (and therefore restore) too.
        let env = env();
        let scripts = [
            emit_posix("demo", &env, false),
            emit_fish("demo", &env, false),
            emit_powershell("demo", &env, false),
        ];
        for name in managed_vars(&env) {
            for script in &scripts {
                assert!(
                    script.contains(&format!("UVR_OLD_{name}")),
                    "{name} not saved in:\n{script}"
                );
            }
        }
    }

    #[test]
    fn a_project_missing_only_the_newer_shims_is_backfilled() {
        // A project initialized before fish/PowerShell support has just
        // `.uvr/activate`; checking that one alone would strand those users.
        let tmp = tempfile::tempdir().unwrap();
        write_shims(tmp.path()).unwrap();
        assert!(!shims_incomplete(tmp.path()));

        std::fs::remove_file(tmp.path().join(DOT_UVR_DIR).join(SHIM_FISH)).unwrap();
        assert!(
            shims_incomplete(tmp.path()),
            "missing fish shim not noticed"
        );

        write_shims(tmp.path()).unwrap();
        assert!(!shims_incomplete(tmp.path()));
    }

    #[test]
    fn prompt_is_untouched_by_default() {
        // Default-off is the whole point: plenty of users build their prompt
        // from a framework that would fight with us.
        let env = env();
        assert!(!emit_posix("demo", &env, false).contains("PS1"));
        assert!(!emit_fish("demo", &env, false).contains("fish_prompt"));
        assert!(!emit_powershell("demo", &env, false).contains("prompt"));
    }

    #[test]
    fn posix_prompt_prefixes_and_restores_ps1() {
        let out = emit_posix("demo", &env(), true);
        assert!(out.contains(r#"PS1='(demo) '"${PS1-}""#), "{out}");
        // Saved before it is modified, and restored on the way out.
        assert!(out.contains(r#"UVR_OLD_PS1="${PS1-}""#));
        assert!(out.contains(r#"if [ -n "${UVR_OLD_PS1_SET-}" ]; then PS1="${UVR_OLD_PS1-}""#));
        assert!(
            out.contains("unset UVR_OLD_PS1 UVR_OLD_PS1_SET")
                || out.contains("UVR_OLD_PS1 UVR_OLD_PS1_SET")
        );
    }

    #[test]
    fn posix_never_exports_the_prompt() {
        // PS1 is a shell variable. Exporting it would push the prompt into
        // every child process, and deactivate could not undo that.
        let out = emit_posix("demo", &env(), true);
        assert!(!out.contains("export PS1"), "PS1 is exported:\n{out}");
        // Other managed vars still are — they are genuine env vars.
        assert!(out.contains("export R_LIBS_USER"));
    }

    #[test]
    fn posix_prompt_save_precedes_the_modification() {
        // If we modified PS1 first, deactivate would "restore" our own
        // prefixed value and the prefix would accumulate.
        let out = emit_posix("demo", &env(), true);
        let save = out.find("UVR_OLD_PS1=").expect("PS1 not saved");
        let apply = out.find("PS1='(demo) '").expect("PS1 not applied");
        assert!(save < apply, "PS1 saved after being modified:\n{out}");
    }

    #[test]
    fn fish_prompt_wraps_and_restores_the_prompt_function() {
        // fish has no PS1 — the prompt is a function, so the old one is
        // copied aside and delegated to.
        let out = emit_fish("demo", &env(), true);
        assert!(
            out.contains("functions -c fish_prompt _uvr_old_fish_prompt"),
            "{out}"
        );
        assert!(out.contains("function fish_prompt"));
        assert!(out.contains(r"printf '%s' '(demo) '"));
        assert!(out.contains("functions -c _uvr_old_fish_prompt fish_prompt"));
        assert!(out.contains("functions -e _uvr_old_fish_prompt"));
    }

    #[test]
    fn powershell_prompt_wraps_and_restores_the_prompt_function() {
        let out = emit_powershell("demo", &env(), true);
        assert!(
            out.contains("$Global:_UVR_OLD_PROMPT = $function:prompt"),
            "{out}"
        );
        assert!(out.contains("function global:prompt { '(demo) ' + (& $Global:_UVR_OLD_PROMPT) }"));
        assert!(out.contains("Set-Item Function:global:prompt $Global:_UVR_OLD_PROMPT"));
    }

    #[test]
    fn powershell_prompt_capture_is_guarded_against_re_activation() {
        // Without the guard, re-activating captures our own wrapper and the
        // prefix nests: "(demo) (demo) > ".
        let out = emit_powershell("demo", &env(), true);
        assert!(
            out.contains("if (-not (Test-Path Variable:Global:_UVR_OLD_PROMPT))"),
            "unguarded prompt capture:\n{out}"
        );
    }

    #[test]
    fn shell_specific_quoting_escapes_embedded_quotes() {
        // fish escapes with a backslash; PowerShell doubles the quote.
        assert_eq!(fish_quote("it's"), r"'it\'s'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        // A Windows path's backslashes must survive fish quoting.
        assert_eq!(fish_quote(r"C:\R\bin"), r"'C:\\R\\bin'");
    }
}
