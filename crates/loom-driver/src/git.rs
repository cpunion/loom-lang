//! Credential-safe Git dependency resolution and checkout caching.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

use crate::DriverError;

const GIT_SOURCE_PREFIX: &str = "git+";

#[derive(Clone, Copy, Debug)]
pub(crate) enum GitSelector<'a> {
    Head,
    Branch(&'a str),
    Tag(&'a str),
    Rev(&'a str),
}

pub(crate) fn selector_identity(selector: GitSelector<'_>) -> String {
    match selector {
        GitSelector::Head => "head".to_owned(),
        GitSelector::Branch(branch) => format!("branch:{branch}"),
        GitSelector::Tag(tag) => format!("tag:{tag}"),
        GitSelector::Rev(revision) => format!("rev:{}", revision.to_ascii_lowercase()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitCheckout {
    pub root: PathBuf,
    pub commit: String,
}

pub(crate) fn validate_request(
    manifest: &Path,
    url: &str,
    selector: GitSelector<'_>,
) -> Result<(), DriverError> {
    validate_url(manifest, url)?;
    match selector {
        GitSelector::Head => Ok(()),
        GitSelector::Branch(branch) => validate_ref(manifest, branch, true),
        GitSelector::Tag(tag) => validate_ref(manifest, tag, false),
        GitSelector::Rev(revision) if is_commit(revision) => Ok(()),
        GitSelector::Rev(_) => Err(git_error(
            manifest,
            "git rev must be a complete 40-character hexadecimal commit",
        )),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Git resolution keeps validation, staging, and atomic publication in one transaction"
)]
pub(crate) fn materialize(
    manifest: &Path,
    project_root: &Path,
    url: &str,
    selector: GitSelector<'_>,
    locked_commit: Option<&str>,
    offline: bool,
) -> Result<GitCheckout, DriverError> {
    validate_request(manifest, url, selector)?;
    if let Some(commit) = locked_commit
        && !is_commit(commit)
    {
        return Err(git_error(manifest, "git lock contains an invalid commit"));
    }

    let commit = if let Some(commit) = locked_commit {
        commit.to_ascii_lowercase()
    } else if let GitSelector::Rev(commit) = selector {
        commit.to_ascii_lowercase()
    } else if offline {
        return Err(git_error(
            manifest,
            "offline git resolution requires an exact rev or an existing lock entry",
        ));
    } else {
        resolve_remote_commit(manifest, url, selector)?
    };

    let url_cache = project_root
        .join("target/loom/git/checkouts")
        .join(url_digest(url));
    let checkout = url_cache.join(&commit);
    if validate_checkout(&checkout, &commit, url) {
        return Ok(GitCheckout {
            root: checkout,
            commit,
        });
    }
    if offline {
        return Err(git_error(
            manifest,
            "offline git dependency has no verified cached checkout",
        ));
    }

    remove_cache_entry(&checkout).map_err(|source| DriverError::Io {
        path: checkout.clone(),
        source,
    })?;
    fs::create_dir_all(&url_cache).map_err(|source| DriverError::Io {
        path: url_cache.clone(),
        source,
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".loom-git-")
        .tempdir_in(&url_cache)
        .map_err(|source| DriverError::Io {
            path: url_cache.clone(),
            source,
        })?;
    let staged_checkout = staging.path().join("checkout");
    run_git(
        manifest,
        [
            OsStr::new("clone"),
            OsStr::new("--no-checkout"),
            OsStr::new("--no-recurse-submodules"),
            OsStr::new("--"),
            OsStr::new(url),
            staged_checkout.as_os_str(),
        ],
        "clone git dependency",
    )?;
    run_git(
        manifest,
        [
            OsStr::new("-C"),
            staged_checkout.as_os_str(),
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new("--force"),
            OsStr::new(&commit),
        ],
        "checkout git dependency commit",
    )?;
    if !validate_checkout(&staged_checkout, &commit, url) {
        return Err(git_error(
            manifest,
            "git produced an unverifiable dependency checkout",
        ));
    }

    match fs::rename(&staged_checkout, &checkout) {
        Ok(()) => {}
        Err(_) if checkout.exists() => {
            if !validate_checkout(&checkout, &commit, url) {
                return Err(git_error(
                    manifest,
                    "concurrent git cache publication produced an invalid checkout",
                ));
            }
        }
        Err(source) => {
            return Err(DriverError::Io {
                path: checkout,
                source,
            });
        }
    }
    Ok(GitCheckout {
        root: checkout,
        commit,
    })
}

pub(crate) fn lock_source(url: &str, commit: &str) -> String {
    format!("{GIT_SOURCE_PREFIX}{url}#{commit}")
}

pub(crate) fn parse_lock_source(source: &str) -> Option<(&str, &str)> {
    let encoded = source.strip_prefix(GIT_SOURCE_PREFIX)?;
    let (url, commit) = encoded.rsplit_once('#')?;
    (!url.is_empty() && is_commit(commit)).then_some((url, commit))
}

pub(crate) fn validate_lock_source(path: &Path, source: &str) -> Result<(), DriverError> {
    let Some((url, _)) = parse_lock_source(source) else {
        return Err(git_error(path, "git lock source is invalid"));
    };
    validate_url(path, url)
}

pub(crate) fn validate_lock_selector(
    path: &Path,
    selector: &str,
    commit: &str,
) -> Result<(), DriverError> {
    if selector == "head" {
        return Ok(());
    }
    if let Some(branch) = selector.strip_prefix("branch:") {
        return validate_ref(path, branch, true);
    }
    if let Some(tag) = selector.strip_prefix("tag:") {
        return validate_ref(path, tag, false);
    }
    if let Some(revision) = selector.strip_prefix("rev:")
        && is_commit(revision)
        && revision.eq_ignore_ascii_case(commit)
    {
        return Ok(());
    }
    Err(git_error(path, "git lock selector is invalid"))
}

fn resolve_remote_commit(
    manifest: &Path,
    url: &str,
    selector: GitSelector<'_>,
) -> Result<String, DriverError> {
    let patterns = match selector {
        GitSelector::Head => vec!["HEAD".to_owned()],
        GitSelector::Branch(branch) => vec![format!("refs/heads/{branch}")],
        GitSelector::Tag(tag) => vec![format!("refs/tags/{tag}"), format!("refs/tags/{tag}^{{}}")],
        GitSelector::Rev(commit) => return Ok(commit.to_ascii_lowercase()),
    };
    let mut arguments = vec![
        OsStr::new("ls-remote"),
        OsStr::new("--exit-code"),
        OsStr::new("--"),
        OsStr::new(url),
    ];
    arguments.extend(patterns.iter().map(OsStr::new));
    let output = run_git(manifest, arguments, "resolve git dependency selector")?;
    let mut candidates = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let mut fields = line.split(u8::is_ascii_whitespace);
            let commit = std::str::from_utf8(fields.next()?).ok()?;
            let reference = std::str::from_utf8(fields.find(|field| !field.is_empty())?).ok()?;
            is_commit(commit).then_some((commit.to_ascii_lowercase(), reference.to_owned()))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.1.cmp(&right.1));
    let selected = match selector {
        GitSelector::Tag(tag) => {
            let peeled = format!("refs/tags/{tag}^{{}}");
            candidates
                .iter()
                .find(|(_, reference)| reference == &peeled)
                .or_else(|| candidates.first())
        }
        GitSelector::Head | GitSelector::Branch(_) => candidates.first(),
        GitSelector::Rev(_) => unreachable!("rev returned before remote resolution"),
    };
    selected.map(|(commit, _)| commit.clone()).ok_or_else(|| {
        git_error(
            manifest,
            "git dependency selector did not resolve to a commit",
        )
    })
}

fn validate_checkout(checkout: &Path, commit: &str, url: &str) -> bool {
    let git_directory = checkout.join(".git");
    let manifest = checkout.join("loom.toml");
    if fs::symlink_metadata(&git_directory).map_or(true, |metadata| !metadata.file_type().is_dir())
        || fs::symlink_metadata(&manifest).map_or(true, |metadata| !metadata.file_type().is_file())
    {
        return false;
    }
    let Ok(head) = git_output([
        OsStr::new("-C"),
        checkout.as_os_str(),
        OsStr::new("rev-parse"),
        OsStr::new("--verify"),
        OsStr::new("HEAD^{commit}"),
    ]) else {
        return false;
    };
    if !head.status.success()
        || std::str::from_utf8(&head.stdout)
            .ok()
            .map(str::trim)
            .is_none_or(|head| !head.eq_ignore_ascii_case(commit))
    {
        return false;
    }
    let Ok(index) = git_output([
        OsStr::new("-C"),
        checkout.as_os_str(),
        OsStr::new("ls-files"),
        OsStr::new("--stage"),
    ]) else {
        return false;
    };
    if !index.status.success()
        || index
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|entry| entry.starts_with(b"160000 "))
    {
        return false;
    }
    let Ok(index_state) = git_output([
        OsStr::new("-C"),
        checkout.as_os_str(),
        OsStr::new("ls-files"),
        OsStr::new("-v"),
        OsStr::new("-z"),
        OsStr::new("--"),
        OsStr::new(":(glob,top)**/*.loom"),
        OsStr::new(":(glob,top)**/loom.toml"),
        OsStr::new(":(exclude,glob,top)**/*_test.loom"),
        OsStr::new(":(exclude,glob,top)target/**"),
        OsStr::new(":(exclude,glob,top)**/target/**"),
    ]) else {
        return false;
    };
    if !index_state.status.success()
        || index_state
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .any(|entry| !entry.starts_with(b"H "))
    {
        return false;
    }
    let Ok(origin) = git_output([
        OsStr::new("-C"),
        checkout.as_os_str(),
        OsStr::new("config"),
        OsStr::new("--local"),
        OsStr::new("--get"),
        OsStr::new("remote.origin.url"),
    ]) else {
        return false;
    };
    if !origin.status.success()
        || std::str::from_utf8(&origin.stdout)
            .ok()
            .map(str::trim)
            .is_none_or(|origin| origin != url)
    {
        return false;
    }
    let Ok(status) = git_output([
        OsStr::new("-C"),
        checkout.as_os_str(),
        OsStr::new("status"),
        OsStr::new("--porcelain=v1"),
        OsStr::new("-z"),
        OsStr::new("--untracked-files=all"),
        OsStr::new("--ignored=matching"),
        OsStr::new("--"),
        OsStr::new(":(glob,top)**/*.loom"),
        OsStr::new(":(glob,top)**/loom.toml"),
        OsStr::new(":(exclude,glob,top)**/*_test.loom"),
        OsStr::new(":(exclude,glob,top)target/**"),
        OsStr::new(":(exclude,glob,top)**/target/**"),
    ]) else {
        return false;
    };
    status.status.success() && status.stdout.is_empty()
}

fn validate_ref(manifest: &Path, reference: &str, branch: bool) -> Result<(), DriverError> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err(git_error(manifest, "git branch or tag selector is invalid"));
    }
    let qualified;
    let arguments = if branch {
        vec![
            OsStr::new("check-ref-format"),
            OsStr::new("--branch"),
            OsStr::new(reference),
        ]
    } else {
        qualified = format!("refs/tags/{reference}");
        vec![OsStr::new("check-ref-format"), OsStr::new(&qualified)]
    };
    let output = git_output(arguments)
        .map_err(|_| git_error(manifest, "cannot start git to validate dependency selector"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error(manifest, "git branch or tag selector is invalid"))
    }
}

fn validate_url(manifest: &Path, url: &str) -> Result<(), DriverError> {
    if url.is_empty()
        || url.contains('#')
        || url.contains('?')
        || url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(git_error(
            manifest,
            "git URL is invalid or contains a query or fragment",
        ));
    }
    if strip_scheme(url, "http://").is_some() {
        return Err(git_error(
            manifest,
            "git dependencies reject plaintext HTTP",
        ));
    }
    if let Some(rest) = strip_scheme(url, "https://") {
        let authority = authority(rest);
        if authority.is_empty() || authority.contains('@') {
            return Err(git_error(
                manifest,
                "HTTPS git URLs must not contain credentials",
            ));
        }
        return Ok(());
    }
    if let Some(rest) = strip_scheme(url, "ssh://") {
        let authority = authority(rest);
        let (user, host) = authority
            .rsplit_once('@')
            .map_or((None, authority), |(user, host)| (Some(user), host));
        if host.is_empty()
            || host.starts_with('-')
            || user.is_some_and(|user| {
                user.is_empty() || user.starts_with('-') || user.contains([':', '@'])
            })
        {
            return Err(git_error(manifest, "SSH git URL is invalid"));
        }
        return Ok(());
    }
    if let Some(rest) = strip_scheme(url, "file://") {
        if rest.is_empty() {
            return Err(git_error(manifest, "file git URL is invalid"));
        }
        return Ok(());
    }
    if !url.contains("://")
        && !url.contains("::")
        && let Some((authority, path)) = url.split_once(':')
    {
        let (user, host) = authority
            .rsplit_once('@')
            .map_or((None, authority), |(user, host)| (Some(user), host));
        if !host.is_empty()
            && !host.starts_with('-')
            && !path.is_empty()
            && !path.starts_with(':')
            && !authority.contains(['/', '\\'])
            && user.is_none_or(|user| {
                !user.is_empty() && !user.starts_with('-') && !user.contains('@')
            })
        {
            return Ok(());
        }
    }
    Err(git_error(
        manifest,
        "git URL must use HTTPS, SSH/scp, or file://",
    ))
}

fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let prefix = url.get(..scheme.len())?;
    prefix
        .eq_ignore_ascii_case(scheme)
        .then(|| &url[scheme.len()..])
}

fn authority(url: &str) -> &str {
    url.split(['/', '?']).next().unwrap_or_default()
}

fn is_commit(commit: &str) -> bool {
    commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn url_digest(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"loom-git-url-v1");
    hasher.update((url.len() as u64).to_le_bytes());
    hasher.update(url.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn run_git<I, S>(manifest: &Path, arguments: I, operation: &str) -> Result<Output, DriverError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_output(arguments)
        .map_err(|_| git_error(manifest, format!("cannot start git to {operation}")))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(git_error(manifest, format!("failed to {operation}")))
    }
}

fn git_output<I, S>(arguments: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.hooksPath=.git/loom-disabled-hooks",
        ])
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_ALLOW_PROTOCOL", "https:ssh:file")
        .output()
}

fn remove_cache_entry(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(path)
        }
        Ok(_) => fs::remove_dir_all(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn git_error(path: &Path, message: impl Into<String>) -> DriverError {
    DriverError::Manifest {
        path: path.to_path_buf(),
        message: message.into(),
    }
}
