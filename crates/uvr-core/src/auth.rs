//! Credentials for authenticated package repositories (#185).
//!
//! A `[[sources]]` entry names a repository; its secret comes from the
//! environment, keyed by that name, or from `~/.netrc`, keyed by host
//! (#186), and never from `uvr.toml`. This is the single credential
//! resolver: the git hosts (GitHub, GitLab, Forgejo) get their tokens from
//! [`GitHost`] here too (#187).

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::{RequestBuilder, Response, StatusCode};

use crate::error::UvrError;
use crate::lockfile::PackageSource;

const TOKEN_PREFIX: &str = "UVR_REPO_TOKEN_";
const USER_PREFIX: &str = "UVR_REPO_USER_";
const PASSWORD_PREFIX: &str = "UVR_REPO_PASSWORD_";
const GIT_TOKEN_PREFIX: &str = "UVR_GIT_TOKEN_";
const GIT_USER_PREFIX: &str = "UVR_GIT_USER_";
/// The user name sent with a `UVR_GIT_TOKEN_<HOST>` token. Bitbucket Cloud
/// access tokens need this name, and GitLab ignores the name.
const DEFAULT_GIT_USER: &str = "x-token-auth";

/// A credential for one repository or git host. `Debug` prints
/// `Bearer ***` / `Basic ***` / `Token ***`, never the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Bearer(String),
    Basic {
        username: String,
        password: String,
    },
    /// `Authorization: token <t>`, the scheme that Forgejo documents.
    Token(String),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Credential::Bearer(_) => "Bearer ***",
            Credential::Basic { .. } => "Basic ***",
            Credential::Token(_) => "Token ***",
        })
    }
}

impl Credential {
    /// Set the request's `Authorization` header. reqwest marks the value
    /// sensitive, and drops it itself on a redirect to another host or port.
    pub fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Credential::Bearer(token) => req.bearer_auth(token),
            Credential::Basic { username, password } => req.basic_auth(username, Some(password)),
            Credential::Token(token) => {
                let value = format!("token {token}");
                match HeaderValue::from_str(&value) {
                    Ok(mut value) => {
                        value.set_sensitive(true);
                        req.header(AUTHORIZATION, value)
                    }
                    // Not a valid header value: reqwest reports that on
                    // send, as it does for bearer_auth.
                    Err(_) => req.header(AUTHORIZATION, value),
                }
            }
        }
    }

    /// The `Authorization` header value, for a client that is not reqwest
    /// (the `git` program, #190).
    pub fn header_value(&self) -> String {
        use base64::Engine;
        match self {
            Credential::Bearer(token) => format!("Bearer {token}"),
            Credential::Basic { username, password } => format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
            ),
            Credential::Token(token) => format!("token {token}"),
        }
    }
}

/// The `<NAME>` part of a repository's credential variables: drop a
/// `:port` suffix, uppercase, and turn every other non-alphanumeric
/// character into `_` (`internal-ppm` → `INTERNAL_PPM`,
/// `ppm.corp.example:8443` → `PPM_CORP_EXAMPLE`).
pub fn env_key(name: &str) -> String {
    let name = name.split_once(':').map_or(name, |(n, _port)| n);
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn read_var(prefix: &str, key: &str) -> Option<String> {
    crate::env_vars::read_env_var(&format!("{prefix}{key}")).map(|v| v.trim().to_string())
}

/// The credential for repository `name` served at `url`. First match wins:
///
/// 1. Credentials in the URL itself (`https://user:pass@host/…`): reqwest
///    sends those on its own, so this returns `None` rather than add a
///    second `Authorization` header.
/// 2. `UVR_REPO_TOKEN_<NAME>` — a bearer token.
/// 3. `UVR_REPO_USER_<NAME>` / `UVR_REPO_PASSWORD_<NAME>` — HTTP basic
///    auth; a half that is not set is sent empty.
/// 4. The netrc entry for the URL's host — HTTP basic auth.
///
/// `<NAME>` is [`env_key`]. Values are trimmed; empty or whitespace-only
/// values count as unset.
pub fn resolve(name: &str, url: &str) -> Option<Credential> {
    if has_userinfo(url) {
        return None;
    }
    env_credential(&env_key(name)).or_else(|| {
        let entry = netrc_entry(&url_host(url)?)?;
        Some(Credential::Basic {
            username: entry.login,
            password: entry.password,
        })
    })
}

fn env_credential(key: &str) -> Option<Credential> {
    if let Some(token) = read_var(TOKEN_PREFIX, key) {
        return Some(Credential::Bearer(token));
    }
    let username = read_var(USER_PREFIX, key);
    let password = read_var(PASSWORD_PREFIX, key);
    if username.is_none() && password.is_none() {
        return None;
    }
    Some(Credential::Basic {
        username: username.unwrap_or_default(),
        password: password.unwrap_or_default(),
    })
}

/// The `host[:port]` of an `https://` or `http://` URL, as the URL writes
/// it. `None` for other URLs, and for a URL with `user@` in it.
pub(crate) fn http_authority(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    (!authority.is_empty() && !authority.contains('@')).then_some(authority)
}

fn url_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_string)
}

/// The password of the netrc entry for git host `host` (no `:port`). The
/// git hosts send it as their API token, in the header each one uses,
/// because GitLab's API does not take basic auth. The login is not used.
fn netrc_password(host: &str) -> Option<String> {
    netrc_entry(host)
        .map(|entry| entry.password)
        .filter(|p| !p.is_empty())
}

/// The netrc file, for messages.
fn netrc_display() -> String {
    netrc_path().map_or_else(|| "~/.netrc".into(), |p| p.display().to_string())
}

/// The netrc entries, as (file, machine), that a git host refused in this
/// run. uvr does not send them again.
static REFUSED_NETRC: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    /// Test servers that stand in for git hosts, as (host, origin). This is
    /// never cleared: it relies on the test harness giving each test a new
    /// thread, as libtest and nextest do.
    static TEST_GIT_ORIGINS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The origin of each URL that uvr builds for git host `host`:
/// `https://<host>`. In unit tests, a test server can take its place on
/// the current thread (`test_git_origin`).
pub(crate) fn git_origin(host: &str) -> String {
    #[cfg(test)]
    {
        let test = TEST_GIT_ORIGINS.with_borrow(|origins| {
            origins
                .iter()
                .find(|(h, _)| h == host)
                .map(|(_, origin)| origin.clone())
        });
        if let Some(origin) = test {
            return origin;
        }
    }
    format!("https://{host}")
}

/// A git host that uvr fetches packages from (#187). This decides which
/// token a host gets, in which header, and which URLs can receive it. The
/// providers only build URLs and call [`GitHost::send`]. A GitLab or
/// Forgejo host is `host[:port]`, so this works for any instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHost<'a> {
    /// github.com, through `api.github.com` and `raw.githubusercontent.com`.
    GitHub,
    GitLab(&'a str),
    Forgejo(&'a str),
    /// Any other host, reached with the `git` program (#190). The value is
    /// the clone URL. Only an `https://` URL gets a credential from uvr.
    Git(&'a str),
}

impl<'a> GitHost<'a> {
    /// The git host that a locked package comes from, if any.
    pub fn for_source(source: &'a PackageSource) -> Option<Self> {
        match source {
            PackageSource::GitHub => Some(GitHost::GitHub),
            PackageSource::Gitlab { host } => Some(GitHost::GitLab(host)),
            PackageSource::Forgejo { host } => Some(GitHost::Forgejo(host)),
            PackageSource::Git { url } => Some(GitHost::Git(url)),
            _ => None,
        }
    }

    /// `host[:port]`. For a `Git` URL that is not `http(s)://`, the URL.
    fn host(&self) -> &'a str {
        match self {
            GitHost::GitHub => "github.com",
            GitHost::GitLab(host) | GitHost::Forgejo(host) => host,
            GitHost::Git(url) => http_authority(url).unwrap_or(url),
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            GitHost::GitHub => "GitHub".into(),
            GitHost::GitLab(host) => format!("GitLab host {host}"),
            GitHost::Forgejo(host) => format!("Forgejo host {host}"),
            GitHost::Git(_) => format!("git host {}", self.host()),
        }
    }

    /// The variables that hold this host's token, in order of precedence.
    /// GitHub: `GITHUB_PAT` (the renv/devtools name), then `GITHUB_TOKEN`
    /// (the CI name). A token also lifts GitHub's anonymous limit of 60
    /// requests per hour (#95). GitLab and Forgejo: the variable for this
    /// host, then the variable for all hosts. `<HOST>` is [`env_key`] of
    /// the host (`git.local:3000` → `GIT_LOCAL`). Any other git host: only
    /// its own variable. A variable for all such hosts would send one token
    /// to every host that a dependency names.
    fn token_vars(&self) -> Vec<String> {
        match self {
            GitHost::GitHub => vec!["GITHUB_PAT".into(), "GITHUB_TOKEN".into()],
            GitHost::GitLab(host) => vec![
                format!("UVR_GITLAB_TOKEN_{}", env_key(host)),
                "UVR_GITLAB_TOKEN".into(),
            ],
            GitHost::Forgejo(host) => vec![
                format!("UVR_FORGEJO_TOKEN_{}", env_key(host)),
                "UVR_FORGEJO_TOKEN".into(),
            ],
            GitHost::Git(_) => vec![format!("{GIT_TOKEN_PREFIX}{}", env_key(self.host()))],
        }
    }

    /// The netrc `machine` of this host: the host name without a port.
    /// GitHub uses `github.com`, the host that its dependencies name.
    fn machine(&self) -> &'a str {
        let host = self.host();
        host.split_once(':').map_or(host, |(h, _port)| h)
    }

    /// Whether `url` is on this host, so that the host's credential can go
    /// to it: the same scheme, host and port as an origin that uvr builds
    /// this host's URLs from. For GitHub, these are `api.github.com` and
    /// `raw.githubusercontent.com` only. `codeload.github.com`, where
    /// tarball requests redirect with their own token in the URL, is not
    /// one of them.
    pub fn serves(&self, url: &str) -> bool {
        let Ok(url) = reqwest::Url::parse(url) else {
            return false;
        };
        let origins = match self {
            GitHost::GitHub => vec![
                git_origin("api.github.com"),
                git_origin("raw.githubusercontent.com"),
            ],
            GitHost::GitLab(host) | GitHost::Forgejo(host) => vec![git_origin(host)],
            GitHost::Git(url) => http_authority(url).map(git_origin).into_iter().collect(),
        };
        origins
            .iter()
            .filter_map(|origin| reqwest::Url::parse(origin).ok())
            .any(|origin| origin.origin() == url.origin())
    }

    /// The first token variable that is set, as (name, value).
    pub(crate) fn env_token(&self) -> Option<(String, String)> {
        self.token_vars().into_iter().find_map(|var| {
            let token = crate::env_vars::read_env_var(&var)?.trim().to_string();
            Some((var, token))
        })
    }

    /// The credential for this host. The first match wins:
    ///
    /// 1. The token variables, in order (see `token_vars`).
    /// 2. The password of the netrc entry for the host (#186), unless the
    ///    host refused it earlier in this run. It must be an access token,
    ///    because GitLab's API does not accept basic auth.
    ///
    /// Forgejo gets `Authorization: token …`. GitHub and GitLab get
    /// `Bearer …`. Values are trimmed, and empty values count as unset.
    ///
    /// Any other git host (`Git`) gets HTTP basic auth, which is what git
    /// servers take, with the token as the password; see `git_credential`.
    pub fn credential(&self) -> Option<Credential> {
        let scheme: fn(String) -> Credential = match self {
            GitHost::Git(url) => return self.git_credential(url),
            GitHost::Forgejo(_) => Credential::Token,
            GitHost::GitHub | GitHost::GitLab(_) => Credential::Bearer,
        };
        let token = match self.env_token() {
            Some((_, token)) => token,
            None if self.netrc_refused() => return None,
            None => netrc_password(self.machine())?,
        };
        Some(scheme(token))
    }

    /// The credential of a `Git` host with an `http(s)://` URL: the token
    /// variable with the user name in `UVR_GIT_USER_<HOST>` (default
    /// `x-token-auth`), else the netrc login and password. The caller sends
    /// it only to URLs that [`GitHost::serves`], which excludes `http://`.
    fn git_credential(&self, url: &str) -> Option<Credential> {
        let host = http_authority(url)?;
        if let Some((_, password)) = self.env_token() {
            let username = read_var(GIT_USER_PREFIX, &env_key(host))
                .unwrap_or_else(|| DEFAULT_GIT_USER.into());
            return Some(Credential::Basic { username, password });
        }
        let entry = netrc_entry(self.machine()).filter(|e| !e.password.is_empty())?;
        Some(Credential::Basic {
            username: Some(entry.login)
                .filter(|login| !login.is_empty())
                .unwrap_or_else(|| DEFAULT_GIT_USER.into()),
            password: entry.password,
        })
    }

    fn netrc_key(&self) -> Option<(PathBuf, String)> {
        Some((netrc_path()?, self.machine().to_ascii_lowercase()))
    }

    fn netrc_refused(&self) -> bool {
        self.netrc_key().is_some_and(|key| {
            REFUSED_NETRC
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&key)
        })
    }

    /// Stop using this host's netrc entry for the rest of the run. Returns
    /// true only the first time.
    fn refuse_netrc(&self) -> bool {
        let Some(key) = self.netrc_key() else {
            return false;
        };
        let mut refused = REFUSED_NETRC.lock().unwrap_or_else(PoisonError::into_inner);
        if refused.contains(&key) {
            return false;
        }
        refused.push(key);
        true
    }

    /// Send `req`. If it goes to this host (see [`GitHost::serves`]), it
    /// carries the host's credential instead of any `Authorization` that it
    /// has. If not, uvr sends it unchanged. reqwest removes the header when
    /// a redirect goes to a different host or port.
    ///
    /// Git and other tools also read netrc, so an entry can hold an account
    /// password or an expired token. The host then refuses it, also for a
    /// public repository, which uvr fetched without credentials before
    /// #186. Thus, when a netrc credential gets a 401 (or a 404, which is
    /// how raw.githubusercontent.com refuses), uvr sends the request again
    /// without credentials. After a 401, or a 404 that the retry turns into
    /// a success, uvr shows one warning and stops using the entry for this
    /// run. If the retry also fails, the result is the first response, so
    /// that a 401 error is about the credential. uvr never drops a token
    /// from a variable like this.
    pub async fn send(&self, req: RequestBuilder) -> reqwest::Result<Response> {
        let (client, request) = req.build_split();
        let mut request = request?;
        let credential = if self.serves(request.url().as_str()) {
            self.credential()
        } else {
            None
        };
        let Some(credential) = credential else {
            return client.execute(request).await;
        };
        request.headers_mut().remove(AUTHORIZATION);
        let anonymous = request.try_clone();
        let resp = credential
            .apply(RequestBuilder::from_parts(client.clone(), request))
            .send()
            .await?;
        let status = resp.status();
        let maybe_refused = status == StatusCode::UNAUTHORIZED || status == StatusCode::NOT_FOUND;
        if !maybe_refused || self.env_token().is_some() {
            return Ok(resp);
        }
        let Some(anonymous) = anonymous else {
            return Ok(resp);
        };
        let retry = client.execute(anonymous).await?;
        // raw.githubusercontent.com answers a refused token with 404, not
        // 401. A 404 counts as a refusal only if the retry then succeeds,
        // because a 404 can also be a file that is not there.
        let refused = status == StatusCode::UNAUTHORIZED || retry.status().is_success();
        if refused && self.refuse_netrc() {
            tracing::warn!(
                "{} refused the password of the `machine {}` entry in {} (HTTP {status}). \
                 uvr continues without it. Put a valid access token in the entry, or set {}.",
                self.label(),
                self.machine(),
                netrc_display(),
                self.token_vars()[0]
            );
        }
        Ok(if retry.status().is_success() {
            retry
        } else {
            resp
        })
    }

    /// What to do about a 401 or 403 from this host. The text never
    /// contains the credential.
    pub fn denied_advice(&self) -> String {
        let vars = self.token_vars();
        let first = &vars[0];
        let machine = self.machine();
        let netrc = netrc_display();
        if let Some((var, _)) = self.env_token() {
            return format!(
                "it refused the token in {var}. Check that the token is valid and can read \
                 this repository."
            );
        }
        if netrc_password(machine).is_some() {
            return format!(
                "it refused the password of the `machine {machine}` entry in {netrc}. Check \
                 that it is a valid access token, or set {first}, which has precedence."
            );
        }
        if let GitHost::Git(_) = self {
            let user = format!("{GIT_USER_PREFIX}{}", env_key(self.host()));
            return format!(
                "if the repository is private, set {first} to an access token (and {user} if \
                 the host needs a user name other than `{DEFAULT_GIT_USER}`), or add a \
                 `machine {machine}` entry with your login and the token as its password to \
                 {netrc}."
            );
        }
        format!(
            "if the repository is private, set {first} (or {}) to an access token, or \
             add a `machine {machine}` entry with the token as its password to {netrc}.",
            vars[1]
        )
    }

    /// The error for a 401 or 403 from this host at `url`, or `None` for
    /// any other status.
    pub fn denied_error(&self, status: StatusCode, url: &str) -> Option<UvrError> {
        if status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN {
            return None;
        }
        Some(UvrError::Other(format!(
            "{} returned HTTP {status} for {}: {}",
            self.label(),
            redact_url(url),
            self.denied_advice()
        )))
    }
}

/// One `machine` entry of a netrc file. No `Debug`: it holds a password.
#[derive(Clone)]
struct NetrcEntry {
    machine: String,
    login: String,
    password: String,
}

/// `$NETRC`, else `~/.netrc`. On Windows, as curl does, `~/_netrc` when
/// there is no `~/.netrc`.
fn netrc_path() -> Option<PathBuf> {
    if let Some(path) = crate::env_vars::read_env_var("NETRC") {
        return Some(PathBuf::from(path));
    }
    let home = dirs::home_dir()?;
    #[cfg(windows)]
    {
        if !home.join(".netrc").exists() {
            return Some(home.join("_netrc"));
        }
    }
    Some(home.join(".netrc"))
}

/// The netrc entry for `host`, compared without case. As in curl, a
/// `machine` is a host name only and never matches a port. A `default`
/// entry is never used: it would send one credential to every repository
/// and git host.
fn netrc_entry(host: &str) -> Option<NetrcEntry> {
    // Read once per process. The key is the path so that tests can point
    // NETRC at a different file.
    static CACHE: Mutex<Option<(PathBuf, Vec<NetrcEntry>)>> = Mutex::new(None);
    let path = netrc_path()?;
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.as_ref().is_none_or(|(cached, _)| *cached != path) {
        let entries = load_netrc(&path);
        *cache = Some((path, entries));
    }
    let (_, entries) = cache.as_ref()?;
    entries
        .iter()
        .find(|e| e.machine.eq_ignore_ascii_case(host))
        .cloned()
}

/// The entries of the netrc file at `path`. A missing file has none. A
/// file that other users can access (Unix), or that uvr cannot read, is
/// skipped with a warning, and the run continues without it.
fn load_netrc(path: &Path) -> Vec<NetrcEntry> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                tracing::warn!(
                    "Ignoring {}: users other than you can access it (mode {mode:o}). \
                     Run `chmod 600 {}` to use it.",
                    path.display(),
                    path.display()
                );
                return Vec::new();
            }
        }
    }
    match std::fs::read_to_string(path) {
        Ok(text) => parse_netrc(&text),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("Ignoring {}: {e}", path.display());
            }
            Vec::new()
        }
    }
}

/// Parse netrc text: whitespace-separated `machine`, `login`, `password`
/// and `account` tokens; `default` (ignored, with the tokens after it);
/// `macdef` bodies (skipped to the next blank line); `#` comments; and
/// curl's double-quoted values with `\"`, `\\`, `\n`, `\r` and `\t`.
fn parse_netrc(text: &str) -> Vec<NetrcEntry> {
    let mut lexer = NetrcLexer(text);
    let mut entries = Vec::new();
    // The entry that `login` and `password` fill: none before the first
    // `machine`, and none in `default`.
    let mut current: Option<NetrcEntry> = None;
    while let Some(keyword) = lexer.keyword() {
        match keyword.as_str() {
            "machine" | "default" => {
                entries.extend(current.take());
                if keyword == "machine" {
                    current = lexer.value().map(|machine| NetrcEntry {
                        machine,
                        login: String::new(),
                        password: String::new(),
                    });
                }
            }
            "login" | "password" | "account" => {
                let value = lexer.value().unwrap_or_default();
                match (current.as_mut(), keyword.as_str()) {
                    (Some(entry), "login") => entry.login = value,
                    (Some(entry), "password") => entry.password = value,
                    _ => {}
                }
            }
            "macdef" => lexer.skip_macdef(),
            _ => {}
        }
    }
    entries.extend(current);
    entries
}

struct NetrcLexer<'a>(&'a str);

impl NetrcLexer<'_> {
    /// The next token in keyword position, after any `#` comment lines.
    fn keyword(&mut self) -> Option<String> {
        loop {
            self.0 = self.0.trim_start();
            if !self.0.starts_with('#') {
                return self.value();
            }
            self.skip_line();
        }
    }

    /// The next token: a bare word, or a double-quoted string, which ends
    /// at its closing quote or at the end of the line.
    fn value(&mut self) -> Option<String> {
        let s = self.0.trim_start();
        if s.is_empty() {
            self.0 = s;
            return None;
        }
        let Some(quoted) = s.strip_prefix('"') else {
            let end = s.find(char::is_whitespace).unwrap_or(s.len());
            self.0 = &s[end..];
            return Some(s[..end].to_string());
        };
        let mut out = String::new();
        let mut end = quoted.len();
        let mut chars = quoted.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    end = i + 1;
                    break;
                }
                '\n' => {
                    end = i;
                    break;
                }
                '\\' => match chars.next() {
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 'r')) => out.push('\r'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, c)) => out.push(c),
                    None => {}
                },
                c => out.push(c),
            }
        }
        self.0 = &quoted[end..];
        Some(out)
    }

    fn skip_line(&mut self) {
        self.0 = self.0.split_once('\n').map_or("", |(_, rest)| rest);
    }

    /// Skip a macro's name and body: all text up to the first blank line.
    fn skip_macdef(&mut self) {
        self.skip_line();
        while !self.0.is_empty() {
            let line = self.0.split_once('\n').map_or(self.0, |(line, _)| line);
            self.skip_line();
            if line.trim().is_empty() {
                break;
            }
        }
    }
}

/// Whether env var `name` holds a repository or `git::` host credential.
pub fn is_credential_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        TOKEN_PREFIX,
        USER_PREFIX,
        PASSWORD_PREFIX,
        GIT_TOKEN_PREFIX,
        GIT_USER_PREFIX,
    ]
    .iter()
    .any(|p| upper.starts_with(p))
}

/// A package repository and the credential the environment gives it.
#[derive(Debug, Clone)]
pub struct Repository {
    pub name: String,
    /// Base URL, without a trailing `/`.
    pub url: String,
    pub credential: Option<Credential>,
}

impl Repository {
    pub fn new(name: &str, url: &str) -> Self {
        Repository {
            name: name.to_string(),
            url: url.trim_end_matches('/').to_string(),
            credential: resolve(name, url),
        }
    }

    /// Whether this repository serves `url`, i.e. `url` is under the
    /// repository URL. This is what keeps a credential on its own
    /// repository: a URL on any other host (or path) never gets it.
    fn serves(&self, url: &str) -> bool {
        url.strip_prefix(&self.url)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    /// The actionable error for a 401/403 from this repository, or `None`
    /// for any other status.
    pub fn denied_error(&self, status: StatusCode) -> Option<UvrError> {
        if status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN {
            return None;
        }
        let key = env_key(&self.name);
        let head = format!(
            "repository '{}' ({}) returned HTTP {status}",
            self.name,
            redact_url(&self.url)
        );
        let host = url_host(&self.url).unwrap_or_default();
        let netrc = netrc_display();
        let advice = match &self.credential {
            None if has_userinfo(&self.url) => format!(
                "it refused the credentials in the repository URL. Check them, or remove them \
                 from the URL and set UVR_REPO_TOKEN_{key} instead."
            ),
            None => format!(
                "it needs credentials. Set UVR_REPO_TOKEN_{key} to a token, or \
                 UVR_REPO_USER_{key} and UVR_REPO_PASSWORD_{key} for HTTP basic auth, \
                 or add a `machine {host}` entry to {netrc}."
            ),
            // A basic credential that the environment did not give came from netrc.
            Some(Credential::Basic { .. }) if env_credential(&key).is_none() => format!(
                "it refused the login and password of the `machine {host}` entry in {netrc}. \
                 Check that they are valid, or set UVR_REPO_TOKEN_{key}, which has precedence."
            ),
            Some(Credential::Bearer(_)) => format!(
                "it refused the token in UVR_REPO_TOKEN_{key}. Check that the token is valid \
                 and gives access to this repository."
            ),
            Some(Credential::Basic { .. }) => format!(
                "it refused the credentials in UVR_REPO_USER_{key} / UVR_REPO_PASSWORD_{key}. \
                 Check that they are valid and give access to this repository."
            ),
            // resolve() never gives this scheme to a repository.
            Some(Credential::Token(_)) => "it refused the token.".into(),
        };
        Some(UvrError::Other(format!("{head}: {advice}")))
    }
}

/// The repository in `repos` that serves `url`. The longest repository URL
/// wins, so a repository nested under another's path keeps its own
/// credential.
pub fn repository_for<'a>(repos: &'a [Repository], url: &str) -> Option<&'a Repository> {
    repos
        .iter()
        .filter(|r| r.serves(url))
        .max_by_key(|r| r.url.len())
}

/// Byte range of the `user:password` part of `url`, if it has one.
fn userinfo_span(url: &str) -> Option<(usize, usize)> {
    let start = url.find("://")? + 3;
    let end = url[start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| start + i);
    let at = url[start..end].rfind('@')?;
    Some((start, start + at))
}

/// Whether `url` carries `user:password@` credentials.
pub fn has_userinfo(url: &str) -> bool {
    userinfo_span(url).is_some()
}

/// `url` with any `user:password@` part replaced by `***@`, for output.
pub fn redact_url(url: &str) -> Cow<'_, str> {
    match userinfo_span(url) {
        Some((start, end)) => Cow::Owned(format!("{}***{}", &url[..start], &url[end..])),
        None => Cow::Borrowed(url),
    }
}

/// Test HTTP server on `127.0.0.1`: answers each request with
/// `respond(request_head)` and records every request head it saw.
/// Not on Windows, for the loopback flakiness the CLI stub server avoids.
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn test_server(
    respond: impl Fn(&str) -> Vec<u8> + Send + 'static,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let reply = respond(&head);
            log.lock().unwrap().push(head);
            let _ = stream.write_all(&reply);
        }
    });
    (url, seen)
}

/// An HTTP/1.1 response for [`test_server`].
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn test_response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// Let `origin` (a [`test_server`]) take the place of git host `host` on
/// this thread. A current-thread tokio runtime runs its tasks there too.
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn test_git_origin(host: &str, origin: &str) {
    TEST_GIT_ORIGINS.with_borrow_mut(|origins| origins.push((host.into(), origin.into())));
}

/// A git host with one private repository, as GitHub, GitLab and Forgejo
/// behave: a request with `Authorization: <auth>` gets `route(path)`,
/// another credential gets 401, and no credential gets 404.
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn test_private_git_host(
    auth: &'static str,
    route: impl Fn(&str) -> Vec<u8> + Send + 'static,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    test_server(move |head| match test_authorization(head) {
        Some(sent) if sent == auth => route(test_path(head)),
        Some(_) => test_response("401 Unauthorized", "", b""),
        None => test_response("404 Not Found", "", b""),
    })
}

/// The path and query of a request head.
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn test_path(head: &str) -> &str {
    head.split_whitespace().nth(1).unwrap_or_default()
}

/// Every variable that the git-host tests set: cleared on creation (with
/// `GITHUB_PAT`, `GITHUB_TOKEN` and `NETRC`), and restored on drop. It
/// holds the env lock, and points `NETRC` at a missing file so that the
/// developer's own `~/.netrc` cannot change a result.
#[cfg(test)]
pub(crate) struct GitEnv {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl GitEnv {
    pub(crate) fn new(vars: &[&'static str]) -> Self {
        let lock = crate::env_vars::env_lock();
        let saved = vars
            .iter()
            .chain(&["GITHUB_PAT", "GITHUB_TOKEN", "NETRC"])
            .map(|var| (*var, std::env::var_os(var)))
            .collect();
        let env = GitEnv { saved, _lock: lock };
        for (var, _) in &env.saved {
            std::env::remove_var(var);
        }
        std::env::set_var("NETRC", "/nonexistent/uvr-test-netrc");
        env
    }
}

#[cfg(test)]
impl Drop for GitEnv {
    fn drop(&mut self) {
        for (var, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }
}

/// The `Authorization` header of a request head, if any.
#[cfg(all(test, not(target_os = "windows")))]
pub(crate) fn test_authorization(head: &str) -> Option<&str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("authorization")
            .then_some(value.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VARS: &[&str] = &[
        "UVR_REPO_TOKEN_INTERNAL_PPM",
        "UVR_REPO_USER_INTERNAL_PPM",
        "UVR_REPO_PASSWORD_INTERNAL_PPM",
    ];

    fn clear() {
        for v in VARS {
            std::env::remove_var(v);
        }
    }

    #[test]
    fn env_key_normalizes_like_forgejo_hosts() {
        assert_eq!(env_key("internal-ppm"), "INTERNAL_PPM");
        assert_eq!(env_key("ppm.corp.example"), "PPM_CORP_EXAMPLE");
        assert_eq!(env_key("127.0.0.1:8443"), "127_0_0_1");
        assert_eq!(env_key("my repo/x"), "MY_REPO_X");
    }

    // One test for every env case: env vars are process-global.
    #[test]
    fn resolve_reads_env_keyed_by_repository() {
        let _env = crate::env_vars::env_lock();
        let url = "https://ppm.corp.example/cran/latest";
        clear();
        assert_eq!(resolve("internal-ppm", url), None);

        // Whitespace-only counts as unset.
        std::env::set_var("UVR_REPO_TOKEN_INTERNAL_PPM", "  ");
        assert_eq!(resolve("internal-ppm", url), None);

        std::env::set_var("UVR_REPO_USER_INTERNAL_PPM", "alice");
        std::env::set_var("UVR_REPO_PASSWORD_INTERNAL_PPM", "s3cret\n");
        assert_eq!(
            resolve("internal-ppm", url),
            Some(Credential::Basic {
                username: "alice".into(),
                password: "s3cret".into()
            })
        );

        // A token beats basic auth.
        std::env::set_var("UVR_REPO_TOKEN_INTERNAL_PPM", " tok123 ");
        assert_eq!(
            resolve("internal-ppm", url),
            Some(Credential::Bearer("tok123".into()))
        );
        // The name, not the host, keys the lookup.
        assert_eq!(resolve("other", url), None);
        // Credentials in the URL are reqwest's to send; never add a second header.
        assert_eq!(
            resolve("internal-ppm", "https://u:p@ppm.corp.example/cran"),
            None
        );

        // Only a password: basic auth with an empty user name.
        std::env::remove_var("UVR_REPO_TOKEN_INTERNAL_PPM");
        std::env::remove_var("UVR_REPO_USER_INTERNAL_PPM");
        assert_eq!(
            resolve("internal-ppm", url),
            Some(Credential::Basic {
                username: String::new(),
                password: "s3cret".into()
            })
        );
        clear();
    }

    #[test]
    fn credential_debug_is_redacted() {
        let bearer = format!("{:?}", Credential::Bearer("tok123".into()));
        let basic = format!(
            "{:?}",
            Repository {
                name: "r".into(),
                url: "https://h".into(),
                credential: Some(Credential::Basic {
                    username: "alice".into(),
                    password: "s3cret".into(),
                }),
            }
        );
        assert_eq!(bearer, "Bearer ***");
        assert!(basic.contains("Basic ***"), "{basic}");
        assert!(
            !basic.contains("alice") && !basic.contains("s3cret"),
            "{basic}"
        );
    }

    #[test]
    fn redact_url_hides_userinfo_only() {
        assert_eq!(
            redact_url("https://alice:s3cret@ppm.corp/cran/src/contrib/a_1.0.tar.gz"),
            "https://***@ppm.corp/cran/src/contrib/a_1.0.tar.gz"
        );
        assert_eq!(redact_url("http://tok@h:8080"), "http://***@h:8080");
        // An `@` in the path is not userinfo.
        let plain = "https://ppm.corp/cran/pkg@1.0?x=a@b";
        assert!(matches!(redact_url(plain), Cow::Borrowed(u) if u == plain));
        assert!(!has_userinfo(plain));
    }

    #[test]
    fn repository_for_matches_by_url_prefix() {
        let repo = |name: &str, url: &str| Repository {
            name: name.into(),
            url: url.into(),
            credential: Some(Credential::Bearer(name.into())),
        };
        let repos = [
            repo("outer", "https://ppm.corp/cran"),
            repo("inner", "https://ppm.corp/cran/internal"),
        ];
        let name = |url: &str| repository_for(&repos, url).map(|r| r.name.as_str());

        assert_eq!(
            name("https://ppm.corp/cran/src/contrib/a.tar.gz"),
            Some("outer")
        );
        assert_eq!(
            name("https://ppm.corp/cran/internal/src/contrib/a.tar.gz"),
            Some("inner")
        );
        // Another host, another port, or a sibling path never gets the credential.
        assert_eq!(name("https://cdn.example/cran/src/contrib/a.tar.gz"), None);
        assert_eq!(
            name("https://ppm.corp:8443/cran/src/contrib/a.tar.gz"),
            None
        );
        assert_eq!(
            name("https://ppm.corp/cran-public/src/contrib/a.tar.gz"),
            None
        );
        assert_eq!(name("http://ppm.corp/cran/src/contrib/a.tar.gz"), None);
    }

    #[test]
    fn denied_error_says_how_to_authenticate() {
        let mut repo = Repository {
            name: "internal-ppm".into(),
            url: "https://ppm.corp/cran".into(),
            credential: None,
        };
        assert!(repo.denied_error(StatusCode::NOT_FOUND).is_none());

        let msg = repo
            .denied_error(StatusCode::UNAUTHORIZED)
            .unwrap()
            .to_string();
        assert!(msg.contains("'internal-ppm'"), "{msg}");
        assert!(msg.contains("401 Unauthorized"), "{msg}");
        assert!(msg.contains("UVR_REPO_TOKEN_INTERNAL_PPM"), "{msg}");
        assert!(msg.contains("UVR_REPO_USER_INTERNAL_PPM"), "{msg}");

        repo.credential = Some(Credential::Bearer("tok123".into()));
        let msg = repo
            .denied_error(StatusCode::FORBIDDEN)
            .unwrap()
            .to_string();
        assert!(msg.contains("403 Forbidden"), "{msg}");
        assert!(
            msg.contains("refused the token in UVR_REPO_TOKEN_INTERNAL_PPM"),
            "{msg}"
        );
        assert!(!msg.contains("tok123"), "{msg}");

        repo.credential = None;
        repo.url = "https://alice:s3cret@ppm.corp/cran".into();
        let msg = repo
            .denied_error(StatusCode::UNAUTHORIZED)
            .unwrap()
            .to_string();
        assert!(msg.contains("credentials in the repository URL"), "{msg}");
        assert!(!msg.contains("s3cret"), "{msg}");
    }

    fn parsed(text: &str) -> Vec<(String, String, String)> {
        parse_netrc(text)
            .into_iter()
            .map(|e| (e.machine, e.login, e.password))
            .collect()
    }

    fn entry(machine: &str, login: &str, password: &str) -> (String, String, String) {
        (machine.into(), login.into(), password.into())
    }

    #[test]
    fn parse_netrc_reads_the_format() {
        let text = "# comment line\n\
            machine ppm.corp login alice password s3cret\n\
            \n\
            macdef init\n\
            machine evil.example login x password y\n\
            \n\
            machine\tgit.corp\r\n  login bob # a comment after a value\n\
            # a comment between tokens\n\
            \x20 password \"two words \\\"q\\\" #x\\\\\" account ignored\n\
            machine nopass.example login carol\n\
            machine \"quoted.example\" password \"unterminated\n\
            login dave\n\
            default login anyone password everywhere\n\
            machine last.example password";
        assert_eq!(
            parsed(text),
            vec![
                entry("ppm.corp", "alice", "s3cret"),
                // A `#` starts a comment only where a keyword can be; in
                // a value it is part of the value.
                entry("git.corp", "bob", "two words \"q\" #x\\"),
                entry("nopass.example", "carol", ""),
                entry("quoted.example", "dave", "unterminated"),
                // `default` is dropped; the file ends before a password.
                entry("last.example", "", ""),
            ]
        );
        assert!(parsed("").is_empty());
        assert!(parsed("default login a password b").is_empty());
        assert_eq!(
            parsed("macdef m\ncd /\nmachine h login l password p"),
            vec![],
            "a macro with no blank line runs to the end"
        );
    }

    /// Write `text` to a netrc file in `dir` with Unix mode `mode`.
    fn write_netrc(dir: &std::path::Path, name: &str, text: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        path
    }

    #[test]
    fn resolve_falls_back_to_netrc() {
        let _env = crate::env_vars::env_lock();
        clear();
        let dir = tempfile::tempdir().unwrap();
        let netrc = write_netrc(
            dir.path(),
            "netrc",
            "machine PPM.corp.example login alice password n3trc-pw\n",
            0o600,
        );
        std::env::set_var("NETRC", &netrc);
        let url = "https://ppm.corp.example:8443/cran/latest";
        let from_netrc = Some(Credential::Basic {
            username: "alice".into(),
            password: "n3trc-pw".into(),
        });

        // Keyed by host (any case, any port), not by the source name.
        assert_eq!(resolve("internal-ppm", url), from_netrc);
        assert_eq!(resolve("other", url), from_netrc);
        assert_eq!(resolve("internal-ppm", "https://cdn.example/cran"), None);
        assert_eq!(
            netrc_password("ppm.corp.example").as_deref(),
            Some("n3trc-pw")
        );
        assert_eq!(netrc_password("cdn.example"), None);
        // URL credentials still win, and no second header is added.
        assert_eq!(
            resolve("internal-ppm", "https://u:p@ppm.corp.example/cran"),
            None
        );

        // A netrc refusal names the entry, never the password.
        let repo = Repository::new("internal-ppm", url);
        let msg = repo
            .denied_error(StatusCode::UNAUTHORIZED)
            .unwrap()
            .to_string();
        assert!(
            msg.contains("`machine ppm.corp.example` entry in")
                && msg.contains(&netrc.display().to_string()),
            "{msg}"
        );
        assert!(!msg.contains("n3trc-pw") && !msg.contains("alice"), "{msg}");

        // Any env credential beats netrc.
        std::env::set_var("UVR_REPO_PASSWORD_INTERNAL_PPM", "env-pw");
        assert_eq!(
            resolve("internal-ppm", url),
            Some(Credential::Basic {
                username: String::new(),
                password: "env-pw".into()
            })
        );
        let msg = Repository::new("internal-ppm", url)
            .denied_error(StatusCode::UNAUTHORIZED)
            .unwrap()
            .to_string();
        assert!(msg.contains("UVR_REPO_PASSWORD_INTERNAL_PPM"), "{msg}");
        std::env::set_var("UVR_REPO_TOKEN_INTERNAL_PPM", "tok");
        assert_eq!(
            resolve("internal-ppm", url),
            Some(Credential::Bearer("tok".into()))
        );
        clear();

        // NETRC is read again when it names another file; the old entry is gone.
        let other = write_netrc(
            dir.path(),
            "other",
            "machine cdn.example password p\n",
            0o600,
        );
        std::env::set_var("NETRC", &other);
        assert_eq!(resolve("internal-ppm", url), None);
        assert_eq!(netrc_password("cdn.example").as_deref(), Some("p"));

        // With no entry, the refusal says how to add one.
        let msg = Repository::new("internal-ppm", url)
            .denied_error(StatusCode::UNAUTHORIZED)
            .unwrap()
            .to_string();
        assert!(
            msg.contains("add a `machine ppm.corp.example` entry to"),
            "{msg}"
        );

        // A missing file is no error.
        std::env::set_var("NETRC", dir.path().join("missing"));
        assert_eq!(resolve("internal-ppm", url), None);
        std::env::remove_var("NETRC");
    }

    /// The token that `host` gets, without its header scheme.
    fn token(host: GitHost) -> Option<String> {
        host.credential().map(|credential| match credential {
            Credential::Bearer(token) | Credential::Token(token) => token,
            Credential::Basic { .. } => panic!("git hosts get no basic auth"),
        })
    }

    // The variables and the precedence that each host had before #187
    // (github_token, gitlab_token and forgejo_token), and #186's netrc
    // fallback: no user has to change their setup.
    #[test]
    fn git_hosts_keep_their_token_variables() {
        let _env = GitEnv::new(&[
            "UVR_FORGEJO_TOKEN",
            "UVR_FORGEJO_TOKEN_LOOKUP_TEST_HOST_EXAMPLE",
            "UVR_FORGEJO_TOKEN_GIT_LOCAL",
            "UVR_GITLAB_TOKEN",
            "UVR_GITLAB_TOKEN_LOOKUP_TEST_HOST_EXAMPLE",
            "UVR_GITLAB_TOKEN_GIT_LOCAL",
        ]);
        let host = "lookup-test-host.example";
        for (kind, forgejo) in [
            (GitHost::Forgejo(host), true),
            (GitHost::GitLab(host), false),
        ] {
            let (per_host, global) = if forgejo {
                (
                    "UVR_FORGEJO_TOKEN_LOOKUP_TEST_HOST_EXAMPLE",
                    "UVR_FORGEJO_TOKEN",
                )
            } else {
                (
                    "UVR_GITLAB_TOKEN_LOOKUP_TEST_HOST_EXAMPLE",
                    "UVR_GITLAB_TOKEN",
                )
            };
            // The per-host variable beats the global one.
            std::env::set_var(per_host, "host-specific");
            std::env::set_var(global, "global");
            assert_eq!(token(kind).as_deref(), Some("host-specific"));
            std::env::remove_var(per_host);
            assert_eq!(token(kind).as_deref(), Some("global"));
            std::env::remove_var(global);
            assert_eq!(token(kind), None);
            // Whitespace-only values count as unset; values are trimmed.
            std::env::set_var(global, "   ");
            assert_eq!(token(kind), None);
            std::env::set_var(global, " tok\n");
            assert_eq!(token(kind).as_deref(), Some("tok"));
            std::env::remove_var(global);
        }
        // A port is not part of the variable name.
        std::env::set_var("UVR_FORGEJO_TOKEN_GIT_LOCAL", "f");
        std::env::set_var("UVR_GITLAB_TOKEN_GIT_LOCAL", "g");
        assert_eq!(
            token(GitHost::Forgejo("git.local:3000")).as_deref(),
            Some("f")
        );
        assert_eq!(
            token(GitHost::GitLab("git.local:3000")).as_deref(),
            Some("g")
        );
        // Each host reads only its own variables.
        assert_eq!(token(GitHost::GitHub), None);
        assert_eq!(token(GitHost::Forgejo("other.local")), None);

        // GitHub: GITHUB_PAT, then GITHUB_TOKEN.
        std::env::set_var("GITHUB_TOKEN", "ci");
        assert_eq!(token(GitHost::GitHub).as_deref(), Some("ci"));
        std::env::set_var("GITHUB_PAT", "pat");
        assert_eq!(token(GitHost::GitHub).as_deref(), Some("pat"));
        std::env::set_var("GITHUB_PAT", " ");
        assert_eq!(token(GitHost::GitHub).as_deref(), Some("ci"));

        // Header schemes: Forgejo `token`, GitHub and GitLab `Bearer`.
        assert_eq!(
            GitHost::Forgejo("git.local").credential(),
            Some(Credential::Token("f".into()))
        );
        assert_eq!(
            GitHost::GitLab("git.local").credential(),
            Some(Credential::Bearer("g".into()))
        );
        assert_eq!(
            GitHost::GitHub.credential(),
            Some(Credential::Bearer("ci".into()))
        );
    }

    #[test]
    fn git_host_tokens_fall_back_to_netrc() {
        let _env = GitEnv::new(&[
            "UVR_FORGEJO_TOKEN",
            "UVR_FORGEJO_TOKEN_GIT_LOCAL",
            "UVR_GITLAB_TOKEN",
            "UVR_GITLAB_TOKEN_GIT_LOCAL",
        ]);
        let dir = tempfile::tempdir().unwrap();
        let netrc = write_netrc(
            dir.path(),
            "netrc",
            "machine git.local login me password pat-local\n\
             machine github.com login me password pat-github\n\
             machine nopass.local login me\n",
            0o600,
        );
        std::env::set_var("NETRC", &netrc);

        // The password is the token; a port is not part of the netrc key.
        assert_eq!(
            token(GitHost::Forgejo("git.local:3000")).as_deref(),
            Some("pat-local")
        );
        assert_eq!(
            token(GitHost::GitLab("git.local")).as_deref(),
            Some("pat-local")
        );
        assert_eq!(token(GitHost::GitHub).as_deref(), Some("pat-github"));
        assert_eq!(token(GitHost::Forgejo("other.local")), None);
        assert_eq!(token(GitHost::GitLab("nopass.local")), None);

        // An env token, per host or global, beats netrc.
        std::env::set_var("UVR_FORGEJO_TOKEN", "env-forgejo");
        std::env::set_var("UVR_GITLAB_TOKEN_GIT_LOCAL", "env-gitlab");
        std::env::set_var("GITHUB_TOKEN", "env-github");
        assert_eq!(
            token(GitHost::Forgejo("git.local")).as_deref(),
            Some("env-forgejo")
        );
        assert_eq!(
            token(GitHost::GitLab("git.local")).as_deref(),
            Some("env-gitlab")
        );
        assert_eq!(token(GitHost::GitHub).as_deref(), Some("env-github"));
    }

    // A git host's credential goes to its own origins only (#187).
    #[test]
    fn git_host_credential_stays_on_its_host() {
        let github = GitHost::GitHub;
        for url in [
            "https://api.github.com/repos/o/r/tarball/abc",
            "https://raw.githubusercontent.com/o/r/abc/DESCRIPTION",
            "https://API.GitHub.com:443/repos/o/r/commits/main",
        ] {
            assert!(github.serves(url), "{url}");
        }
        for url in [
            // Where the tarball endpoint redirects: its URL has its own token.
            "https://codeload.github.com/o/r/legacy.tar.gz/abc?token=x",
            "https://github.com/o/r",
            "https://gitlab.com/api/v4/projects/1",
            "https://packagemanager.posit.co/cran/latest/bin/a.tgz",
            "http://api.github.com/repos/o/r/tarball/abc",
            "https://api.github.com:8443/repos/o/r/tarball/abc",
            "https://api.github.com.evil.example/repos/o/r",
            "https://api.github.com@evil.example/repos/o/r",
            "https://evil.example/api.github.com/repos/o/r",
            "not a url",
        ] {
            assert!(!github.serves(url), "{url}");
        }

        let forgejo = GitHost::Forgejo("git.local:3000");
        assert!(forgejo.serves("https://git.local:3000/api/v1/repos/o/r/archive/a.tar.gz"));
        assert!(forgejo.serves("https://GIT.LOCAL:3000/api/v1/x"));
        assert!(!forgejo.serves("https://git.local/api/v1/x"));
        assert!(!forgejo.serves("https://git.local:3001/api/v1/x"));
        assert!(!forgejo.serves("http://git.local:3000/api/v1/x"));
        assert!(!forgejo.serves("https://api.github.com/repos/o/r/tarball/abc"));

        let gitlab = GitHost::GitLab("gitlab.com");
        assert!(gitlab.serves("https://gitlab.com:443/api/v4/projects/1"));
        assert!(!gitlab.serves("https://gitlab.com:8443/api/v4/projects/1"));
        assert!(!gitlab.serves("https://codefloe.com/api/v1/repos/o/r"));
        assert!(!GitHost::Forgejo("codefloe.com").serves("https://gitlab.com/api/v4/x"));

        assert_eq!(
            GitHost::for_source(&PackageSource::Forgejo {
                host: "git.local:3000".into()
            }),
            Some(forgejo)
        );
        assert_eq!(
            GitHost::for_source(&PackageSource::Gitlab {
                host: "gitlab.com".into()
            }),
            Some(gitlab)
        );
        assert_eq!(GitHost::for_source(&PackageSource::GitHub), Some(github));
        assert_eq!(GitHost::for_source(&PackageSource::Cran), None);

        // #190: a `git::` host is the https origin of its clone URL.
        let url = "https://git.corp.example/team/repo.git";
        let git = GitHost::Git(url);
        assert!(git.serves(url));
        assert!(git.serves("https://GIT.corp.example:443/other/repo.git"));
        assert!(!git.serves("http://git.corp.example/team/repo.git"));
        assert!(!git.serves("https://git.corp.example:8443/team/repo.git"));
        assert!(!git.serves("https://api.github.com/repos/o/r"));
        for url in [
            "http://git.corp.example/team/repo.git",
            "ssh://git@git.corp.example/team/repo.git",
            "git@git.corp.example:team/repo.git",
            "file:///srv/repo.git",
        ] {
            assert!(!GitHost::Git(url).serves(url), "{url}");
        }
        assert_eq!(
            GitHost::for_source(&PackageSource::Git { url: url.into() }),
            Some(git)
        );
        assert!(!GitHost::GitLab("git.corp.example").serves("ssh://git.corp.example/x"));
    }

    #[test]
    fn git_host_advice_names_its_own_variables() {
        let _env = GitEnv::new(&["UVR_GIT_TOKEN_GIT_CORP_EXAMPLE"]);
        let host = GitHost::Git("https://git.corp.example:8443/team/repo.git");
        assert_eq!(host.label(), "git host git.corp.example:8443");
        let msg = host.denied_advice();
        assert!(
            msg.contains("set UVR_GIT_TOKEN_GIT_CORP_EXAMPLE to an access token")
                && msg.contains("UVR_GIT_USER_GIT_CORP_EXAMPLE")
                && msg.contains("`machine git.corp.example` entry"),
            "{msg}"
        );
        assert!(!msg.contains("UVR_GITLAB") && !msg.contains("(or"), "{msg}");
        std::env::set_var("UVR_GIT_TOKEN_GIT_CORP_EXAMPLE", "s3cret");
        let msg = host.denied_advice();
        assert!(
            msg.contains("refused the token in UVR_GIT_TOKEN_GIT_CORP_EXAMPLE."),
            "{msg}"
        );
        assert!(!msg.contains("s3cret"), "{msg}");
        assert_eq!(
            format!("{:?}", host.credential().unwrap()),
            "Basic ***",
            "the token is basic auth, and Debug hides it"
        );
    }

    #[test]
    fn token_header_is_forgejo_scheme_and_sensitive() {
        let request = Credential::Token("tok123".into())
            .apply(reqwest::Client::new().get("https://git.local/"))
            .build()
            .unwrap();
        let value = &request.headers()[AUTHORIZATION];
        assert_eq!(value, "token tok123");
        assert!(value.is_sensitive());
        assert_eq!(
            format!("{:?}", Credential::Token("tok123".into())),
            "Token ***"
        );
        // An invalid value fails the request, as bearer_auth does.
        assert!(Credential::Token("a\nb".into())
            .apply(reqwest::Client::new().get("https://git.local/"))
            .build()
            .is_err());
    }

    #[test]
    fn git_host_refusal_says_how_to_authenticate() {
        let _env = GitEnv::new(&["UVR_FORGEJO_TOKEN_GIT_LOCAL", "UVR_FORGEJO_TOKEN"]);
        let host = GitHost::Forgejo("git.local:3000");
        let url = "https://git.local:3000/api/v1/repos/o/r/archive/a.tar.gz";
        assert!(host.denied_error(StatusCode::NOT_FOUND, url).is_none());

        let msg = host
            .denied_error(StatusCode::UNAUTHORIZED, url)
            .unwrap()
            .to_string();
        assert!(msg.contains("Forgejo host git.local:3000"), "{msg}");
        assert!(
            msg.contains("set UVR_FORGEJO_TOKEN_GIT_LOCAL (or UVR_FORGEJO_TOKEN)"),
            "{msg}"
        );
        assert!(msg.contains("`machine git.local` entry"), "{msg}");
        assert!(!msg.contains("UVR_REPO_"), "{msg}");

        std::env::set_var("UVR_FORGEJO_TOKEN", "s3cret-tok");
        let msg = host
            .denied_error(StatusCode::FORBIDDEN, url)
            .unwrap()
            .to_string();
        assert!(
            msg.contains("refused the token in UVR_FORGEJO_TOKEN."),
            "{msg}"
        );
        assert!(!msg.contains("s3cret-tok"), "{msg}");
        std::env::remove_var("UVR_FORGEJO_TOKEN");

        let dir = tempfile::tempdir().unwrap();
        let netrc = write_netrc(
            dir.path(),
            "netrc",
            "machine github.com password n3trc-tok\n",
            0o600,
        );
        std::env::set_var("NETRC", &netrc);
        let msg = GitHost::GitHub.denied_advice();
        assert!(
            msg.contains("refused the password of the `machine github.com` entry in")
                && msg.contains(&netrc.display().to_string())
                && msg.contains("set GITHUB_PAT, which has precedence"),
            "{msg}"
        );
        assert!(!msg.contains("n3trc-tok"), "{msg}");
    }

    #[cfg(unix)]
    #[test]
    fn netrc_that_other_users_can_read_is_skipped() {
        let _env = crate::env_vars::env_lock();
        clear();
        let dir = tempfile::tempdir().unwrap();
        for (name, mode) in [("world", 0o644), ("group", 0o640)] {
            let path = write_netrc(
                dir.path(),
                name,
                "machine ppm.corp.example login alice password n3trc-pw\n",
                mode,
            );
            std::env::set_var("NETRC", &path);
            assert_eq!(
                resolve("internal-ppm", "https://ppm.corp.example/cran"),
                None
            );
            assert_eq!(netrc_password("ppm.corp.example"), None);
        }
        std::env::remove_var("NETRC");
    }

    #[test]
    fn credential_vars_are_recognized() {
        assert!(is_credential_var("UVR_REPO_TOKEN_X"));
        assert!(is_credential_var("UVR_REPO_USER_X"));
        assert!(is_credential_var("uvr_repo_password_x"));
        assert!(is_credential_var("UVR_GIT_TOKEN_GIT_CORP"));
        assert!(is_credential_var("UVR_GIT_USER_GIT_CORP"));
        assert!(!is_credential_var("UVR_REPOS"));
        assert!(!is_credential_var("GITHUB_PAT"));
    }
}
