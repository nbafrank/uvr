//! Packages from any git host, given as `git::<clone URL>` (#190).
//!
//! There is no host API to call, so uvr runs the `git` program:
//! `git ls-remote` turns a branch or tag into a commit, and a shallow fetch
//! of that commit gives the tree. (`git archive --remote` would be cheaper,
//! but GitHub and most other hosts refuse it over https.) Fetching a commit
//! by its SHA needs protocol v2, which GitHub, GitLab, Bitbucket and
//! Forgejo all serve.
//!
//! The tree goes into the download cache as a `.tar.gz` whose top-level
//! directory is the package name, keyed by URL and commit. `uvr add`,
//! `uvr lock` and every later `uvr sync` of a commit use that one fetch.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use semver::Version;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::auth::GitHost;
use crate::error::{Result, UvrError};
use crate::lockfile::PackageSource;
use crate::manifest::RemoteEntry;
use crate::registry::PackageInfo;

/// uvr gives git the credential in `GIT_CONFIG_*` variables, which need
/// git 2.31 or later.
const MIN_GIT_FOR_CREDENTIALS: (u32, u32) = (2, 31);

/// A `core.hooksPath` with no hooks in it.
const NO_HOOKS: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// Variables that would point git at a repository other than uvr's own.
const REPOSITORY_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

/// A parsed `git::<url>[@ref]` spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSpec {
    pub url: String,
    pub git_ref: Option<String>,
}

/// Parse `git::<url>[@ref]` (the `git::` prefix is optional), or say what
/// is wrong with it. As in the `remotes` package, `@ref` starts at the
/// first `@` after the first `/` that follows the host, so
/// `git@host:team/repo.git@v1` and `https://host/repo.git@feature/x` split
/// correctly. An scp-like URL with no `/` (`git@host:repo.git@v1`) splits
/// at the first `@` after its last `:`, because a ref cannot hold `:`.
/// [`validate_url`] lists the URLs that uvr accepts.
pub fn parse_git_parts(spec: &str) -> std::result::Result<GitSpec, String> {
    let body = spec.strip_prefix("git::").unwrap_or(spec);
    let host_start = body.find("://").map_or(0, |i| i + 3);
    let path_start = body[host_start..]
        .find('/')
        .map(|slash| host_start + slash)
        .or_else(|| {
            body.rfind(':')
                .filter(|_| host_start == 0)
                .map(|colon| colon + 1)
        });
    let at = path_start.and_then(|path| body[path..].find('@').map(|at| path + at));
    let (url, git_ref) = match at {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    validate_url(url)?;
    if let Some(git_ref) = git_ref {
        if !is_valid_ref(git_ref) {
            return Err(format!("`{git_ref}` is not a valid git ref"));
        }
    }
    Ok(GitSpec {
        url: url.to_string(),
        git_ref: git_ref.map(str::to_string),
    })
}

/// The spec of a manifest dependency `git = "git::<url>"` with an optional
/// `rev`. The ref is in `rev` or after `@` in `git`, not in both. uvr does
/// not join them as `<url>@<rev>`, so the URL split rules of
/// [`parse_git_parts`] do not apply to `rev`.
pub fn manifest_spec(git: &str, rev: Option<&str>) -> std::result::Result<GitSpec, String> {
    let mut spec = parse_git_parts(git)?;
    if let Some(rev) = rev {
        if spec.git_ref.is_some() {
            return Err("the ref is given twice: after `@` in `git`, and in `rev`".into());
        }
        if !is_valid_ref(rev) {
            return Err(format!("`{rev}` is not a valid git ref"));
        }
        spec.git_ref = Some(rev.to_string());
    }
    Ok(spec)
}

/// A ref that can go to `git ls-remote`: a branch, tag, `HEAD` or full ref
/// name (`refs/...`), or a commit SHA.
pub fn is_valid_ref(git_ref: &str) -> bool {
    crate::registry::github::is_valid_git_ref(git_ref) && !git_ref.starts_with('-')
}

/// Check a clone URL. uvr accepts `https://host/path`,
/// `ssh://[user@]host[:port]/path` and the scp-like `[user@]host:path`, and
/// also, with a warning when it is used, `http://` (no transport security,
/// and uvr sends it no credential) and `file:///path` (the lockfile then
/// works on this machine only).
///
/// It refuses other schemes; a URL that starts with `-`, which git would
/// read as an option; whitespace, control characters, `#` and `?`; `@` in
/// the path, where `@ref` goes; and credentials: any `user@` in an
/// `http(s)` URL, or a password in any URL, because `uvr.toml` and
/// `uvr.lock` would then hold the secret.
pub fn validate_url(url: &str) -> std::result::Result<(), String> {
    if url.is_empty() {
        return Err("the clone URL is empty".into());
    }
    if url.starts_with('-') {
        return Err("a clone URL cannot start with `-`".into());
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '#' || c == '?')
    {
        return Err("a clone URL cannot contain whitespace, `#` or `?`".into());
    }
    // `<helper>::<address>` makes git run `git-remote-<helper>`.
    if let Some(helper) = url.find("::").filter(|&i| !url[..i].contains('/')) {
        return Err(format!(
            "`{}::` URLs are not supported; use https://, ssh:// or file://",
            &url[..helper]
        ));
    }
    let no_path = || "the clone URL has no repository path".to_string();
    let (userinfo, host, path) = match url.split_once("://") {
        Some(("https" | "http" | "ssh", rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let (userinfo, host) = match authority.rsplit_once('@') {
                Some((userinfo, host)) => (Some(userinfo), host),
                None => (None, authority),
            };
            if userinfo.is_some() && !url.starts_with("ssh://") {
                return Err(format!(
                    "the clone URL has credentials in it, which uvr would save in uvr.toml and \
                     uvr.lock. Remove them and set {} instead",
                    token_var_for(host)
                ));
            }
            (userinfo, host, path)
        }
        Some(("file", rest)) => {
            let path = rest.strip_prefix('/').ok_or("expected `file:///<path>`")?;
            (None, "localhost", path)
        }
        Some((scheme, _)) => {
            return Err(format!(
                "`{scheme}://` URLs are not supported; use https://, ssh:// or file://"
            ))
        }
        None => {
            let (user_host, path) = url
                .split_once(':')
                .filter(|(user_host, _)| !user_host.contains('/'))
                .ok_or(
                    "expected an https://, ssh:// or file:// URL, or the scp-like form \
                     user@host:path",
                )?;
            match user_host.rsplit_once('@') {
                Some((userinfo, host)) => (Some(userinfo), host, path),
                None => (None, user_host, path),
            }
        }
    };
    if host.is_empty() {
        return Err("the clone URL has no host".into());
    }
    if userinfo.is_some_and(|u| u.is_empty() || u.contains([':', '@'])) {
        return Err("the clone URL has a password in it; use ssh keys instead".into());
    }
    if path.contains('@') {
        return Err("`@` in a clone URL path must start the ref (`<url>@<ref>`)".into());
    }
    if path.trim_matches('/').is_empty() {
        return Err(no_path());
    }
    Ok(())
}

fn token_var_for(host: &str) -> String {
    format!("UVR_GIT_TOKEN_{}", crate::auth::env_key(host))
}

/// The repository name in a clone URL: its last path segment without
/// `.git` (`https://host/team/pkg.git` → `pkg`).
pub fn repo_name(url: &str) -> &str {
    let last = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last)
}

/// A full commit SHA: 40 (SHA-1) or 64 (SHA-256) hex digits.
fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// The `git` program on `path` (a `PATH` value).
pub fn find_git(path: Option<OsString>) -> Result<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    which::which_in("git", path, cwd).map_err(|_| {
        UvrError::Other(
            "uvr needs the `git` program for `git::` dependencies, but `git` is not on PATH. \
             Install git (https://git-scm.com/downloads) and try again."
                .into(),
        )
    })
}

/// `(major, minor)` from `git --version` (`git version 2.39.3 (Apple Git-146)`).
fn git_version(program: &Path) -> Option<(u32, u32)> {
    let out = Command::new(program).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace().nth(2)?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The URLs that uvr warned about in this run.
static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn warn_once(url: &str) {
    let message = if url.starts_with("http://") {
        "uses plain http: anyone on the network can change what uvr installs, and uvr sends \
         no credential to it. Use https"
    } else if url.starts_with("file://") {
        "is a path on this machine: uvr.lock will not work on another machine"
    } else {
        return;
    };
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    if !warned.iter().any(|u| u == url) {
        warned.push(url.to_string());
        tracing::warn!("The git dependency {url} {message}.");
    }
}

/// The `GIT_CONFIG_*` variables that give git the credential for `url`:
/// an `http.<origin>/.extraHeader`, so git sends it only to that origin.
/// None for a URL that the host does not serve (`http://`, ssh, file), or
/// when uvr has no credential: git then uses its own credential helpers.
/// `GIT_CONFIG_*` entries that the user set stay in place.
fn credential_env(
    url: &str,
    version: impl FnOnce() -> Option<(u32, u32)>,
) -> Result<Vec<(String, String)>> {
    let host = GitHost::Git(url);
    if !host.serves(url) {
        return Ok(Vec::new());
    }
    let Some(credential) = host.credential() else {
        return Ok(Vec::new());
    };
    let found = version();
    if found.is_none_or(|v| v < MIN_GIT_FOR_CREDENTIALS) {
        let found = found.map_or("an unknown version".into(), |(a, b)| format!("{a}.{b}"));
        if host.env_token().is_none() {
            // A netrc entry: git reads ~/.netrc itself.
            debug!("git {found} is too old for GIT_CONFIG_*; git reads ~/.netrc itself");
            return Ok(Vec::new());
        }
        return Err(UvrError::Other(format!(
            "uvr gives git the token for {url} in GIT_CONFIG_* variables, which need git 2.31 \
             or later, but git is {found}. Upgrade git, or unset the token variable and let \
             git's own credential helper answer."
        )));
    }
    let origin = reqwest::Url::parse(url)
        .map_err(|e| UvrError::Other(format!("invalid clone URL {url}: {e}")))?
        .origin()
        .ascii_serialization();
    let index = crate::env_vars::read_env_var("GIT_CONFIG_COUNT")
        .and_then(|n| n.trim().parse::<usize>().ok())
        .unwrap_or(0);
    Ok(vec![
        ("GIT_CONFIG_COUNT".into(), (index + 1).to_string()),
        (
            format!("GIT_CONFIG_KEY_{index}"),
            format!("http.{origin}/.extraHeader"),
        ),
        (
            format!("GIT_CONFIG_VALUE_{index}"),
            format!("Authorization: {}", credential.header_value()),
        ),
    ])
}

/// How uvr runs `git` for one clone URL.
struct GitCmd {
    program: PathBuf,
    /// The credential, if any (see `credential_env`). Never printed.
    env: Vec<(String, String)>,
    /// Whether the URL is one that uvr can give a credential to.
    serves: bool,
}

impl GitCmd {
    /// Build this on the calling thread, before `spawn_blocking`: in unit
    /// tests, `GitHost::serves` reads per-thread test origins.
    fn for_url(url: &str) -> Result<Self> {
        let program = find_git(std::env::var_os("PATH"))?;
        warn_once(url);
        let env = credential_env(url, || git_version(&program))?;
        Ok(GitCmd {
            program,
            env,
            serves: GitHost::Git(url).serves(url),
        })
    }

    /// `git` with prompts, hooks and the `ext::` transport off, protocol v2
    /// (which can fetch any commit by SHA), and no repository from the
    /// environment. The caller passes URLs after `--`.
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(["-c", "protocol.version=2", "-c", "protocol.ext.allow=never"])
            .arg("-c")
            .arg(format!("core.hooksPath={NO_HOOKS}"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        for var in REPOSITORY_VARS {
            cmd.env_remove(var);
        }
        cmd
    }

    /// Run git and return its stdout. The error says that git could not
    /// `what` the repository at `url`, with git's own message.
    fn run<S: AsRef<OsStr>>(&self, url: &str, what: &str, args: &[S]) -> Result<Vec<u8>> {
        let out = self.command().args(args).output().map_err(|e| {
            UvrError::Other(format!("could not run {}: {e}", self.program.display()))
        })?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        Err(self.error(url, what, &String::from_utf8_lossy(&out.stderr)))
    }

    /// git's message, and what to do if the host refused access.
    fn error(&self, url: &str, what: &str, stderr: &str) -> UvrError {
        const REFUSED: &[&str] = &[
            "could not read Username",
            "could not read Password",
            "Authentication failed",
            "Access denied",
            "returned error: 401",
            "returned error: 403",
        ];
        let advice = if self.serves && REFUSED.iter().any(|m| stderr.contains(m)) {
            let host = GitHost::Git(url);
            format!("\n{}: {}", host.label(), host.denied_advice())
        } else if stderr.contains("Permission denied") && !url.starts_with("http") {
            "\nuvr sends no credential for this URL: git uses your own ssh keys and agent. \
             Check that you can clone it with git."
                .to_string()
        } else {
            String::new()
        };
        UvrError::Other(format!(
            "git could not {what} {url}:\n{}{advice}",
            stderr.trim()
        ))
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| UvrError::Other(format!("git task failed: {e}")))?
}

/// The refs that `git_ref` can name, in the order git itself prefers them:
/// a peeled annotated tag, a tag, then a branch. Full names (`refs/...`),
/// not the short name, so `ls-remote` does not also list
/// `refs/heads/feature/<ref>`.
fn ref_candidates(git_ref: &str) -> Vec<String> {
    if git_ref == "HEAD" {
        vec!["HEAD".into()]
    } else if git_ref.starts_with("refs/") {
        vec![format!("{git_ref}^{{}}"), git_ref.into()]
    } else {
        vec![
            format!("refs/tags/{git_ref}^{{}}"),
            format!("refs/tags/{git_ref}"),
            format!("refs/heads/{git_ref}"),
        ]
    }
}

/// The commit of the first candidate in `git ls-remote` output.
fn pick_ref(listing: &str, candidates: &[String]) -> Option<String> {
    candidates.iter().find_map(|want| {
        listing.lines().find_map(|line| {
            let (sha, name) = line.split_once('\t')?;
            (name == want && is_object_id(sha)).then(|| sha.to_ascii_lowercase())
        })
    })
}

/// The commit that `git_ref` names in the repository at `url`: `HEAD`, a
/// tag, a branch, a full ref name, or a full commit SHA. A full SHA needs
/// no request. An abbreviated SHA is not accepted: the host lists refs,
/// not commits.
pub async fn fetch_commit_sha(url: &str, git_ref: &str) -> Result<String> {
    if is_object_id(git_ref) {
        return Ok(git_ref.to_ascii_lowercase());
    }
    validate_url(url).map_err(|e| UvrError::Other(format!("git dependency {url}: {e}")))?;
    if !is_valid_ref(git_ref) {
        return Err(UvrError::Other(format!(
            "git dependency {url}: `{git_ref}` is not a valid git ref"
        )));
    }
    let candidates = ref_candidates(git_ref);
    let git = GitCmd::for_url(url)?;
    let mut args = vec!["ls-remote".to_string(), "--".into(), url.into()];
    args.extend(candidates.iter().cloned());
    let owned_url = url.to_string();
    let listing = blocking(move || git.run(&owned_url, "list the refs of", &args)).await?;
    let listing = String::from_utf8_lossy(&listing);
    pick_ref(&listing, &candidates).ok_or_else(|| {
        let hint = if git_ref.chars().all(|c| c.is_ascii_hexdigit()) {
            " To pin a commit, give its full 40-character SHA."
        } else {
            ""
        };
        UvrError::Other(format!(
            "git ref `{git_ref}` was not found in {url}: it is not a branch or tag there.{hint}"
        ))
    })
}

/// The cache file for `commit` of `url`.
fn tarball_path(cache_dir: &Path, url: &str, commit: &str) -> PathBuf {
    let tag = hex::encode(Sha256::digest(url.as_bytes()));
    cache_dir.join(format!("{}-git-{commit}.tar.gz", &tag[..8]))
}

/// The repository tree at `commit` as a `.tar.gz` in `cache_dir`. Its one
/// top-level directory is the package name from the root `DESCRIPTION`, so
/// the install path can read the package from it. A cached file is used
/// while it matches its `.sha256` sidecar, so a commit is fetched once.
pub async fn cached_tarball(cache_dir: &Path, url: &str, commit: &str) -> Result<PathBuf> {
    if !is_object_id(commit) {
        return Err(UvrError::Other(format!(
            "git dependency {url}: `{commit}` is not a full commit SHA"
        )));
    }
    let commit = commit.to_ascii_lowercase();
    let dest = tarball_path(cache_dir, url, &commit);
    if crate::installer::download::sidecar_cache_hit(&dest, url)? {
        debug!("Cache hit: {url}@{commit}");
        return Ok(dest);
    }
    validate_url(url).map_err(|e| UvrError::Other(format!("git dependency {url}: {e}")))?;
    let git = GitCmd::for_url(url)?;
    let (url, cache_dir) = (url.to_string(), cache_dir.to_path_buf());
    blocking(move || fetch_tarball(&git, &url, &commit, &cache_dir, dest)).await
}

fn fetch_tarball(
    git: &GitCmd,
    url: &str,
    commit: &str,
    cache_dir: &Path,
    dest: PathBuf,
) -> Result<PathBuf> {
    use flate2::{write::GzEncoder, Compression};

    std::fs::create_dir_all(cache_dir)?;
    let repo = tempfile::Builder::new()
        .prefix(".uvr-git-")
        .tempdir_in(cache_dir)?;
    let git_dir = repo.path().as_os_str();
    let in_repo = |args: &[&OsStr]| -> Vec<OsString> {
        [OsStr::new("--git-dir"), git_dir]
            .iter()
            .chain(args)
            .map(|a| a.to_os_string())
            .collect()
    };
    git.run(
        url,
        "create a repository for",
        &[
            OsStr::new("init"),
            OsStr::new("-q"),
            OsStr::new("--bare"),
            git_dir,
        ],
    )?;
    debug!("Fetching {url}@{commit}");
    git.run(
        url,
        &format!("fetch commit {commit} from"),
        &in_repo(&["fetch", "-q", "--depth=1", "--", url, commit].map(OsStr::new)),
    )?;
    let description = git
        .run(
            url,
            "read DESCRIPTION from",
            &in_repo(&["cat-file", "blob", &format!("{commit}:DESCRIPTION")].map(OsStr::new)),
        )
        .map_err(|e| {
            UvrError::Other(format!(
                "{url} has no DESCRIPTION file at the repository root at commit {commit} \
                 (packages in a subdirectory are not supported for git:: dependencies): {e}"
            ))
        })?;
    let prefix = archive_prefix(&String::from_utf8_lossy(&description), url);

    let mut child = git
        .command()
        .args(in_repo(
            &[
                "archive",
                "--format=tar",
                &format!("--prefix={prefix}/"),
                commit,
            ]
            .map(OsStr::new),
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".uvr-dl-")
        .tempfile_in(cache_dir)?;
    let mut gz = GzEncoder::new(
        std::io::BufWriter::new(tmp.as_file_mut()),
        Compression::default(),
    );
    // git archive writes little to stderr, so reading stdout first cannot
    // block on a full stderr pipe.
    let copied = child
        .stdout
        .take()
        .map(|mut stdout| std::io::copy(&mut stdout, &mut gz));
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(git.error(url, "archive", &String::from_utf8_lossy(&out.stderr)));
    }
    if let Some(copied) = copied {
        copied?;
    }
    gz.finish()?.into_inner().map_err(|e| e.into_error())?;
    tmp.persist(&dest).map_err(|e| {
        UvrError::Other(format!(
            "Failed to persist {} to {}: {e}",
            url,
            dest.display()
        ))
    })?;
    let bytes = std::fs::read(&dest)?;
    crate::installer::download::write_sidecar(
        &dest.with_extension("sha256"),
        &crate::checksum::sha256_hex(&bytes),
        url,
    );
    Ok(dest)
}

/// The top-level directory of the tarball: the `Package:` name when it is
/// valid, else the repository name, else `package`. Never a path.
fn archive_prefix(description: &str, url: &str) -> String {
    let fields = crate::dcf::parse_dcf_fields(description);
    let declared = fields.get("Package").map(|p| p.trim());
    let name = [declared, Some(repo_name(url))]
        .into_iter()
        .flatten()
        .find(|name| crate::package_name::is_valid(name))
        .unwrap_or("package");
    name.to_string()
}

/// The `DESCRIPTION` at the top of a tarball from [`cached_tarball`].
fn read_description(tarball: &Path) -> Result<String> {
    let file = std::fs::File::open(tarball)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        let mut parts = path.components();
        if parts.nth(1).is_some_and(|c| c.as_os_str() == "DESCRIPTION") && parts.next().is_none() {
            let mut text = String::new();
            entry.take(1024 * 1024).read_to_string(&mut text)?;
            return Ok(text);
        }
    }
    Err(UvrError::Other(format!(
        "{} has no DESCRIPTION file",
        tarball.display()
    )))
}

/// Resolve the package at `commit` of the repository at `url`: its
/// `PackageInfo`, its `Remotes:` entries, and the names of its install-time
/// dependencies (for binding nested `Remotes:`). With
/// `require_declared_name`, DESCRIPTION must declare a valid `Package:`.
pub async fn resolve_git_package_at_commit_bound(
    cache_dir: &Path,
    url: &str,
    commit: &str,
    require_declared_name: bool,
) -> Result<(PackageInfo, Vec<RemoteEntry>, BTreeSet<String>)> {
    let tarball = cached_tarball(cache_dir, url, commit).await?;
    let commit = commit.to_ascii_lowercase();
    let fields = crate::dcf::parse_dcf_fields(&read_description(&tarball)?);
    let name = description_package_name(&fields, url, &commit, require_declared_name)?;
    let raw_version = fields
        .get("Version")
        .cloned()
        .unwrap_or_else(|| "0.0.0".to_string());
    let version = Version::parse(&crate::resolver::normalize_version(&raw_version))
        .unwrap_or_else(|_| Version::new(0, 0, 0));
    let remotes = fields
        .get("Remotes")
        .map(|field| crate::manifest::parse_remotes_field_rich(field))
        .unwrap_or_default();
    debug!("git {url}@{commit} → {name} {version}");
    Ok((
        PackageInfo {
            name,
            version,
            source: PackageSource::Git {
                url: url.to_string(),
            },
            checksum: Some(format!("git:{commit}")),
            requires: crate::registry::github::parse_description_runtime_deps(&fields),
            // The clone URL is the source; the lockfile needs no `url`.
            url: String::new(),
            raw_version: None,
            system_requirements: None,
            subdirectory: None,
        },
        remotes,
        crate::registry::github::parse_description_install_dependency_names(&fields),
    ))
}

fn description_package_name(
    fields: &BTreeMap<String, String>,
    url: &str,
    commit: &str,
    require_declared_name: bool,
) -> Result<String> {
    let declared = fields
        .get("Package")
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    if !require_declared_name {
        return Ok(declared.unwrap_or_else(|| repo_name(url).to_string()));
    }
    let name = declared.ok_or_else(|| {
        UvrError::Other(format!(
            "DESCRIPTION for {url}@{commit} has no `Package:` field; cannot validate the bound \
             package identity."
        ))
    })?;
    if !crate::package_name::is_valid(&name) {
        return Err(UvrError::Other(format!(
            "DESCRIPTION for {url}@{commit} declares an invalid `Package:` name '{name}'."
        )));
    }
    Ok(name)
}

/// A local repository to use as a `git::` remote in tests: `gitpkg`,
/// with commit `first` (tagged `light` and, annotated, `v0.1.0`) and a
/// later commit `second` on `main`, which is `HEAD`. `None` without git.
#[cfg(test)]
pub(crate) struct TestRepo {
    pub dir: tempfile::TempDir,
    pub url: String,
    pub first: String,
    pub second: String,
}

#[cfg(test)]
impl TestRepo {
    pub fn new() -> Option<Self> {
        if find_git(std::env::var_os("PATH")).is_err() {
            eprintln!("skipping: git is not on PATH");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| Self::git_in(dir.path(), args);
        let write = |path: &str, text: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        run(&["init", "-q"]);
        run(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        write(
            "DESCRIPTION",
            "Package: gitpkg\nVersion: 0.1.0\nTitle: Test\nDescription: A test package.\n\
             License: MIT\nAuthor: uvr\nMaintainer: uvr <uvr@example.com>\n\
             Imports: jsonlite\nLinkingTo: cpp11\nNeedsCompilation: no\n\
             Remotes: owner/jsonlite@v1\n",
        );
        write("NAMESPACE", "export(hello)\n");
        write("R/hello.R", "hello <- function() \"hi\"\n");
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "first"]);
        run(&["tag", "light"]);
        run(&["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);
        let first = run(&["rev-parse", "HEAD"]);
        write(
            "DESCRIPTION",
            "Package: gitpkg\nVersion: 0.2.0\nTitle: Test\nDescription: A test package.\n\
             License: MIT\nAuthor: uvr\nMaintainer: uvr <uvr@example.com>\n\
             NeedsCompilation: no\n",
        );
        run(&["commit", "-q", "-am", "second"]);
        let second = run(&["rev-parse", "HEAD"]);
        let path = dir.path().to_string_lossy().replace('\\', "/");
        let url = format!("file:///{}", path.trim_start_matches('/'));
        Some(TestRepo {
            dir,
            url,
            first,
            second,
        })
    }

    /// Run git in `dir`, without the user's hooks or signing, and return
    /// its trimmed stdout.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let mut cmd = Command::new(find_git(std::env::var_os("PATH")).unwrap());
        for var in REPOSITORY_VARS {
            cmd.env_remove(var);
        }
        let out = cmd
            .args(["-c", "user.name=uvr", "-c", "user.email=uvr@example.com"])
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .arg("-c")
            .arg(format!("core.hooksPath={NO_HOOKS}"))
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    pub fn git(&self, args: &[&str]) -> String {
        Self::git_in(self.dir.path(), args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str, git_ref: Option<&str>) -> GitSpec {
        GitSpec {
            url: url.into(),
            git_ref: git_ref.map(str::to_string),
        }
    }

    #[test]
    fn parse_accepts_clone_urls_and_splits_the_ref() {
        for (raw, url, git_ref) in [
            (
                "git::https://git.corp.example/team/repo.git",
                "https://git.corp.example/team/repo.git",
                None,
            ),
            (
                "git::https://git.corp.example:8443/team/sub/repo.git@v1.2.0",
                "https://git.corp.example:8443/team/sub/repo.git",
                Some("v1.2.0"),
            ),
            (
                "git::https://host/repo@feature/x",
                "https://host/repo",
                Some("feature/x"),
            ),
            (
                "git::ssh://git@host:2222/team/repo.git@main",
                "ssh://git@host:2222/team/repo.git",
                Some("main"),
            ),
            (
                "git::git@bitbucket.org:team/repo.git@abc123",
                "git@bitbucket.org:team/repo.git",
                Some("abc123"),
            ),
            ("host:repo.git", "host:repo.git", None),
            // No `/` in the path (gitolite style).
            ("git::git@host:repo.git@v1", "git@host:repo.git", Some("v1")),
            ("git::http://host/repo.git", "http://host/repo.git", None),
            (
                "git::file:///srv/git/repo.git",
                "file:///srv/git/repo.git",
                None,
            ),
            (
                "git::file:///C:/git/repo@v1",
                "file:///C:/git/repo",
                Some("v1"),
            ),
            // An IPv6 host is not a `<helper>::` URL.
            (
                "git::https://[::1]:8443/repo.git",
                "https://[::1]:8443/repo.git",
                None,
            ),
        ] {
            assert_eq!(parse_git_parts(raw), Ok(spec(url, git_ref)), "{raw}");
        }
    }

    #[test]
    fn parse_rejects_unsafe_or_unsupported_urls() {
        for (raw, reason) in [
            ("git::", "empty"),
            ("git::-uhttps://host/repo.git", "cannot start with `-`"),
            ("git::--upload-pack=touch /tmp/x", "cannot start with `-`"),
            ("git::https://tok@host/repo.git", "UVR_GIT_TOKEN_HOST"),
            ("git::https://u:p@host/repo.git", "credentials"),
            ("git::http://u@host:80/repo.git", "UVR_GIT_TOKEN_HOST"),
            ("git::ssh://git:pw@host/repo.git", "password"),
            ("git::user:pw@host:repo.git", "`@` in a clone URL path"),
            ("git::ext::sh -c touch% /tmp/x", "whitespace"),
            ("git::ext::sh", "`ext::` URLs are not supported"),
            ("git::fd::17/x", "`fd::` URLs are not supported"),
            (
                "git::git://host/repo.git",
                "`git://` URLs are not supported",
            ),
            ("git::ftp://host/repo.git", "not supported"),
            ("git::https://host/repo.git#subdirectory=pkg", "`#`"),
            ("git::https://host/repo.git?x=1", "`?`"),
            ("git::https://host", "no repository path"),
            ("git::https:///repo.git", "no host"),
            ("git::file://relative/repo", "file:///"),
            ("git::./local/repo", "scp-like"),
            ("git::/abs/repo.git", "scp-like"),
            ("git::https://host/repo.git@", "not a valid git ref"),
            ("git::https://host/repo.git@-x", "not a valid git ref"),
            ("git::https://host/repo.git@a..b", "not a valid git ref"),
            ("git::https://host/repo.git@a b", "not a valid git ref"),
        ] {
            let err = parse_git_parts(raw).expect_err(raw);
            assert!(err.contains(reason), "{raw}: {err}");
        }
    }

    #[test]
    fn manifest_spec_keeps_rev_apart_from_the_url() {
        // Joined as `<url>@<rev>`, a URL with `@` in it would split wrongly.
        assert_eq!(
            manifest_spec("git::git@host:repo.git", Some("v1")),
            Ok(spec("git@host:repo.git", Some("v1")))
        );
        assert_eq!(
            manifest_spec("git::https://h/r.git@v1", None),
            Ok(spec("https://h/r.git", Some("v1")))
        );
        assert_eq!(
            manifest_spec("git::https://h/r.git", None),
            Ok(spec("https://h/r.git", None))
        );
        let twice = manifest_spec("git::https://h/r.git@v1", Some("v2")).unwrap_err();
        assert!(twice.contains("given twice"), "{twice}");
        assert!(manifest_spec("git::https://h/r.git", Some("a b")).is_err());
        assert!(manifest_spec("git::https://h/r.git", Some("-x")).is_err());
    }

    #[test]
    fn repo_name_is_the_last_segment_without_dot_git() {
        assert_eq!(repo_name("https://host/team/pkg.git"), "pkg");
        assert_eq!(repo_name("https://host/team/pkg/"), "pkg");
        assert_eq!(repo_name("git@host:pkg.git"), "pkg");
        assert_eq!(repo_name("ssh://git@host/team/my.pkg"), "my.pkg");
    }

    #[test]
    fn missing_git_is_a_clear_error() {
        let empty = tempfile::tempdir().unwrap();
        let err = find_git(Some(empty.path().into())).unwrap_err().to_string();
        assert!(
            err.contains("`git` is not on PATH") && err.contains("git::"),
            "{err}"
        );
    }

    #[test]
    fn ref_listing_is_matched_exactly() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = "c".repeat(40);
        let listing = format!(
            "{a}\trefs/heads/feature/main\n{b}\trefs/heads/main\n{c}\trefs/tags/v1\n\
             {a}\trefs/tags/v1^{{}}\n{c}\trefs/remotes/origin/HEAD\n{b}\tHEAD\n"
        );
        let pick = |r: &str| pick_ref(&listing, &ref_candidates(r));
        assert_eq!(pick("main"), Some(b.clone()));
        assert_eq!(pick("HEAD"), Some(b.clone()));
        // An annotated tag resolves to its commit, not the tag object.
        assert_eq!(pick("v1"), Some(a.clone()));
        assert_eq!(pick("refs/tags/v1"), Some(a));
        assert_eq!(pick("refs/heads/main"), Some(b));
        assert_eq!(pick("feature"), None);
        assert_eq!(pick("origin/HEAD"), None);
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn refs_resolve_to_commits() {
        let Some(repo) = TestRepo::new() else { return };
        let rt = runtime();
        let resolve = |r: &str| rt.block_on(fetch_commit_sha(&repo.url, r));
        assert_eq!(resolve("HEAD").unwrap(), repo.second);
        assert_eq!(resolve("main").unwrap(), repo.second);
        assert_eq!(resolve("refs/heads/main").unwrap(), repo.second);
        assert_eq!(resolve("light").unwrap(), repo.first);
        assert_eq!(resolve("v0.1.0").unwrap(), repo.first);

        let err = resolve("no-such-branch").unwrap_err().to_string();
        assert!(err.contains("`no-such-branch` was not found"), "{err}");
        let short = &repo.first[..7];
        let err = resolve(short).unwrap_err().to_string();
        assert!(err.contains("full 40-character SHA"), "{err}");

        // A full SHA needs no request, so the repository need not exist.
        let upper = repo.first.to_ascii_uppercase();
        assert_eq!(
            rt.block_on(fetch_commit_sha("https://nowhere.invalid/r.git", &upper))
                .unwrap(),
            repo.first
        );
    }

    #[test]
    fn package_resolves_at_a_commit_and_is_fetched_once() {
        let Some(repo) = TestRepo::new() else { return };
        let rt = runtime();
        let cache = tempfile::tempdir().unwrap();
        let resolve = |commit: &str, bound: bool| {
            rt.block_on(resolve_git_package_at_commit_bound(
                cache.path(),
                &repo.url,
                commit,
                bound,
            ))
        };

        let (info, remotes, install) = resolve(&repo.first, true).unwrap();
        assert_eq!(info.name, "gitpkg");
        assert_eq!(info.version.to_string(), "0.1.0");
        assert_eq!(
            info.source,
            PackageSource::Git {
                url: repo.url.clone()
            }
        );
        assert_eq!(info.checksum, Some(format!("git:{}", repo.first)));
        assert_eq!(info.url, "");
        assert_eq!(
            info.requires
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["jsonlite"]
        );
        assert!(install.contains("cpp11") && install.contains("jsonlite"));
        assert!(
            matches!(&remotes[..], [RemoteEntry::Source(s)] if s.repository == "owner/jsonlite"),
            "{remotes:?}"
        );
        let (info, remotes, _) = resolve(&repo.second, false).unwrap();
        assert_eq!(info.version.to_string(), "0.2.0");
        assert!(remotes.is_empty());

        // The tarball holds the package under its name.
        let tarball = tarball_path(cache.path(), &repo.url, &repo.first);
        assert_eq!(
            crate::installer::binary_install::inspect_tarball(&tarball, "gitpkg")
                .map(|meta| meta.pure_r),
            Some(true)
        );

        // With the repository gone, the cached commits still resolve.
        let dir = repo.dir.path().to_path_buf();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(resolve(&repo.first, true).unwrap().0.name, "gitpkg");
        // A commit that was never fetched fails, and says what git said.
        let other = "0123456789abcdef0123456789abcdef01234567";
        let err = resolve(other, true).unwrap_err().to_string();
        assert!(
            err.contains(&format!("git could not fetch commit {other} from")),
            "{err}"
        );
    }

    #[test]
    fn unknown_commit_and_missing_description_fail_clearly() {
        let Some(repo) = TestRepo::new() else { return };
        let rt = runtime();
        let cache = tempfile::tempdir().unwrap();
        let other = "0123456789abcdef0123456789abcdef01234567";
        let err = rt
            .block_on(cached_tarball(cache.path(), &repo.url, other))
            .unwrap_err()
            .to_string();
        assert!(err.contains("fetch commit"), "{err}");

        let err = rt
            .block_on(cached_tarball(cache.path(), &repo.url, "main"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a full commit SHA"), "{err}");

        // A repository without DESCRIPTION at its root.
        repo.git(&["mv", "DESCRIPTION", "OTHER"]);
        repo.git(&["commit", "-q", "-m", "no description"]);
        let head = rt.block_on(fetch_commit_sha(&repo.url, "main")).unwrap();
        let err = rt
            .block_on(cached_tarball(cache.path(), &repo.url, &head))
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no DESCRIPTION file"), "{err}");
    }

    #[test]
    fn bound_names_must_be_declared_and_valid() {
        let url = "https://host/team/repo.git";
        let fields = |text: &str| crate::dcf::parse_dcf_fields(text);
        let err = description_package_name(&fields("Version: 1\n"), url, "c", true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no `Package:` field"), "{err}");
        let err = description_package_name(&fields("Package: a b\n"), url, "c", true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid `Package:` name"), "{err}");
        assert_eq!(
            description_package_name(&fields("Version: 1\n"), url, "c", false).unwrap(),
            "repo"
        );
        // The tarball directory is never a path from DESCRIPTION.
        assert_eq!(archive_prefix("Package: ../../x\n", url), "repo");
        assert_eq!(archive_prefix("Package: ../../x\n", "git@h:.."), "package");
        assert_eq!(archive_prefix("Package: pkg\n", url), "pkg");
    }

    // The credential reaches git only for an https URL on its own host,
    // in the environment, never in the arguments.
    #[test]
    fn credential_is_passed_for_https_urls_only() {
        use crate::auth::GitEnv;

        let _env = GitEnv::new(&[
            "UVR_GIT_TOKEN_GIT_CORP_EXAMPLE",
            "UVR_GIT_USER_GIT_CORP_EXAMPLE",
            "GIT_CONFIG_COUNT",
        ]);
        let url = "https://git.corp.example/team/repo.git";
        let new = || Some((2, 31));
        assert!(credential_env(url, new).unwrap().is_empty());

        std::env::set_var("UVR_GIT_TOKEN_GIT_CORP_EXAMPLE", "s3cret");
        // base64("x-token-auth:s3cret")
        let header = "Authorization: Basic eC10b2tlbi1hdXRoOnMzY3JldA==";
        let pairs = |env: Vec<(String, String)>| {
            env.into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            pairs(credential_env(url, new).unwrap()),
            [
                "GIT_CONFIG_COUNT=1".to_string(),
                "GIT_CONFIG_KEY_0=http.https://git.corp.example/.extraHeader".into(),
                format!("GIT_CONFIG_VALUE_0={header}"),
            ]
        );
        // The same host on another port has the same variable (as for
        // GitLab), but the header is for that origin only.
        let env = pairs(credential_env("https://git.corp.example:8443/r.git", new).unwrap());
        assert_eq!(
            env[1],
            "GIT_CONFIG_KEY_0=http.https://git.corp.example:8443/.extraHeader"
        );
        // Other schemes and hosts get nothing.
        for other in [
            "http://git.corp.example/team/repo.git",
            "ssh://git@git.corp.example/team/repo.git",
            "git@git.corp.example:team/repo.git",
            "file:///git.corp.example/repo.git",
            "https://other.example/team/repo.git",
        ] {
            assert!(credential_env(other, new).unwrap().is_empty(), "{other}");
        }

        // A user name, and GIT_CONFIG_* entries that the user set.
        std::env::set_var("UVR_GIT_USER_GIT_CORP_EXAMPLE", "alice");
        std::env::set_var("GIT_CONFIG_COUNT", "2");
        let env = pairs(credential_env(url, new).unwrap());
        // base64("alice:s3cret")
        assert_eq!(
            env,
            [
                "GIT_CONFIG_COUNT=3".to_string(),
                "GIT_CONFIG_KEY_2=http.https://git.corp.example/.extraHeader".into(),
                "GIT_CONFIG_VALUE_2=Authorization: Basic YWxpY2U6czNjcmV0".into(),
            ]
        );
        std::env::remove_var("GIT_CONFIG_COUNT");

        // git before 2.31 ignores GIT_CONFIG_*: say so instead of a 401.
        let err = credential_env(url, || Some((2, 30)))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("need git 2.31") && err.contains("git is 2.30"),
            "{err}"
        );
        assert!(!err.contains("s3cret"), "{err}");
        assert!(credential_env(url, || None).is_err());

        // A command never carries the credential in its arguments.
        let git = GitCmd {
            program: "git".into(),
            env: credential_env(url, new).unwrap(),
            serves: true,
        };
        let cmd = git.command();
        assert!(cmd
            .get_args()
            .all(|a| !a.to_string_lossy().contains("Authorization")));
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "GIT_TERMINAL_PROMPT" && v == Some(OsStr::new("0"))));
        assert!(cmd.get_envs().any(|(k, v)| k == "GIT_DIR" && v.is_none()));
    }

    #[test]
    fn netrc_credential_is_basic_auth_with_its_login() {
        use crate::auth::GitEnv;

        let _env = GitEnv::new(&["UVR_GIT_TOKEN_GIT_CORP_EXAMPLE", "GIT_CONFIG_COUNT"]);
        let dir = tempfile::tempdir().unwrap();
        let netrc = dir.path().join("netrc");
        std::fs::write(
            &netrc,
            "machine git.corp.example login bob password n3trc\nmachine nologin.example password p\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&netrc, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::env::set_var("NETRC", &netrc);
        let credential = |url: &str| GitHost::Git(url).credential();
        assert_eq!(
            credential("https://git.corp.example:8443/r.git"),
            Some(crate::auth::Credential::Basic {
                username: "bob".into(),
                password: "n3trc".into()
            })
        );
        assert_eq!(
            credential("https://nologin.example/r.git"),
            Some(crate::auth::Credential::Basic {
                username: "x-token-auth".into(),
                password: "p".into()
            })
        );
        assert_eq!(credential("git@git.corp.example:r.git"), None);
        // Too old a git for GIT_CONFIG_*: git reads ~/.netrc on its own.
        assert!(
            credential_env("https://git.corp.example/r.git", || Some((2, 20)))
                .unwrap()
                .is_empty()
        );
    }

    // #190 with #187: a private repository on an http(s) git server. A
    // test server that speaks git's "dumb" HTTP protocol takes the place of
    // an https host, and lists refs only for the right basic credential.
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn private_repository_resolves_with_the_token() {
        use crate::auth::{
            test_authorization, test_git_origin, test_path, test_response, test_server, GitEnv,
        };

        let sha = "0123456789abcdef0123456789abcdef01234567";
        let _env = GitEnv::new(&["UVR_GIT_TOKEN_127_0_0_1", "GIT_CONFIG_COUNT"]);
        let git = match find_git(std::env::var_os("PATH")) {
            Ok(git) => git,
            Err(_) => return,
        };
        if git_version(&git).is_none_or(|v| v < MIN_GIT_FOR_CREDENTIALS) {
            eprintln!("skipping: git is older than 2.31");
            return;
        }
        let (origin, seen) = test_server(move |head| {
            // base64("x-token-auth:git-tok")
            if test_authorization(head) != Some("Basic eC10b2tlbi1hdXRoOmdpdC10b2s=") {
                return test_response(
                    "401 Unauthorized",
                    "WWW-Authenticate: Basic realm=\"git\"\r\n",
                    b"",
                );
            }
            match test_path(head) {
                p if p.starts_with("/team/repo.git/info/refs") => test_response(
                    "200 OK",
                    "Content-Type: text/plain\r\n",
                    format!("{sha}\trefs/heads/main\n").as_bytes(),
                ),
                "/team/repo.git/HEAD" => test_response("200 OK", "", b"ref: refs/heads/main\n"),
                _ => test_response("404 Not Found", "", b""),
            }
        });
        let authority = origin.trim_start_matches("http://").to_string();
        test_git_origin(&authority, &origin);
        let url = format!("{origin}/team/repo.git");
        let rt = runtime();
        let resolve = || rt.block_on(fetch_commit_sha(&url, "main"));

        let err = resolve().unwrap_err().to_string();
        assert!(
            err.contains("git could not list the refs of")
                && err.contains("set UVR_GIT_TOKEN_127_0_0_1 to an access token"),
            "{err}"
        );
        std::env::set_var("UVR_GIT_TOKEN_127_0_0_1", "wrong-tok");
        let err = resolve().unwrap_err().to_string();
        assert!(
            err.contains("refused the token in UVR_GIT_TOKEN_127_0_0_1"),
            "{err}"
        );
        assert!(!err.contains("wrong-tok"), "{err}");

        std::env::set_var("UVR_GIT_TOKEN_127_0_0_1", "git-tok");
        assert_eq!(resolve().unwrap(), sha);
        let heads = seen.lock().unwrap();
        assert!(heads
            .iter()
            .any(|h| test_authorization(h) == Some("Basic eC10b2tlbi1hdXRoOmdpdC10b2s=")));
    }
}
