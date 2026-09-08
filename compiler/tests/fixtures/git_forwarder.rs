//! Trusted test executable: checks Loom's process boundary, then forwards to real
//! Git. Only the fixture HTTPS URL is remapped; object storage and plumbing stay
//! Git's responsibility. Constants survive the resolver's cleared environment.

use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::Path,
    process::{self, Command},
};

const ROOT: &str = env!("LOOM_GIT_FIXTURE_ROOT");
const GIT: &str = env!("LOOM_GIT_FIXTURE_REAL_GIT");
const SENTINEL: &str = "LOOM_GIT_REMOTE_SENTINEL_secret";

fn require(condition: bool, message: &str) {
    if !condition {
        eprintln!("Git fixture policy violation: {message}");
        process::exit(90);
    }
}

fn git_path(path: &Path) -> String {
    let path = path.to_str().unwrap();
    if !cfg!(windows) {
        return path.to_owned();
    }
    let path = path.replace('\\', "/");
    if let Some(path) = path.strip_prefix("//?/UNC/") {
        return format!("//{path}");
    }
    if let Some(drive) = path.strip_prefix("//?/") {
        if drive.as_bytes().get(1) == Some(&b':') && drive.as_bytes().get(2) == Some(&b'/') {
            return drive.to_owned();
        }
    }
    path
}

fn main() {
    let root = Path::new(ROOT);
    let current = env::current_dir().unwrap().canonicalize().unwrap();
    require(
        current.starts_with(root),
        "working directory escaped fixture",
    );
    let home = current.join("home");
    let config = current.join("empty-config");
    let empty = current.join("empty");
    let child_home = git_path(&home);
    let child_config = git_path(&config);
    let child_empty = git_path(&empty);
    let repository = git_path(&current.join("repository"));
    for name in ["HOME", "USERPROFILE", "XDG_CONFIG_HOME"] {
        require(
            env::var(name).as_deref() == Ok(child_home.as_str()),
            "unisolated home",
        );
    }
    for name in ["NETRC", "GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM"] {
        require(
            env::var(name).as_deref() == Ok(child_config.as_str()),
            "unisolated configuration",
        );
    }
    require(
        fs::read(&config).unwrap().is_empty(),
        "configuration is not empty",
    );
    require(
        fs::read_dir(&home).unwrap().next().is_none(),
        "home is not empty",
    );
    require(
        fs::read_dir(&empty).unwrap().next().is_none(),
        "hook directory is not empty",
    );
    for (name, value) in [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_ALLOW_PROTOCOL", "https"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("LC_ALL", "C"),
    ] {
        require(
            env::var(name).as_deref() == Ok(value),
            "missing isolation environment",
        );
    }
    let allowed = [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "NETRC",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_ALLOW_PROTOCOL",
        "GIT_TERMINAL_PROMPT",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_NO_LAZY_FETCH",
        "LC_ALL",
    ];
    for (name, _) in env::vars_os() {
        require(
            allowed.iter().any(|allowed| {
                name.to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(allowed))
            }),
            "ambient environment leaked",
        );
    }

    let mut args = env::args().skip(1).collect::<Vec<_>>();
    require(
        args.iter().any(|arg| arg == "--no-pager"),
        "pager not disabled",
    );
    require(
        args.iter().any(|arg| arg == "--no-replace-objects"),
        "object replacement not disabled",
    );
    let settings = args
        .windows(2)
        .filter(|pair| pair[0] == "-c")
        .map(|pair| pair[1].split_once('=').unwrap())
        .collect::<BTreeMap<_, _>>();
    for (name, value) in [
        ("credential.helper", ""),
        ("credential.interactive", "false"),
        ("http.sslVerify", "true"),
        ("http.followRedirects", "false"),
        ("http.proxy", ""),
        ("http.emptyAuth", "false"),
        ("http.delegation", "none"),
        ("fetch.fsckObjects", "true"),
        ("gc.auto", "0"),
        ("maintenance.auto", "false"),
    ] {
        require(
            settings.get(name).copied() == Some(value),
            "missing isolation setting",
        );
    }
    require(
        settings.get("core.hooksPath").copied() == Some(child_empty.as_str()),
        "hooks not isolated",
    );
    if cfg!(windows) {
        require(
            settings.get("core.longpaths").copied() == Some("true"),
            "Git builtin long paths not enabled",
        );
    }
    let mut offset = 0;
    while offset < args.len() {
        if args[offset] == "-c" {
            offset += 2;
        } else if args[offset].starts_with("--") {
            if let Some(path) = args[offset].strip_prefix("--git-dir=") {
                require(path == repository, "unexpected Git directory");
            }
            offset += 1;
        } else {
            break;
        }
    }
    let command = args.get(offset).map(String::as_str).unwrap_or("");
    require(
        matches!(command, "init" | "fetch" | "ls-tree" | "cat-file"),
        "unexpected Git command",
    );
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("git.log"))
        .unwrap();
    writeln!(log, "{}", args[offset..].join("\t")).unwrap();
    if root.join("offline").exists() {
        eprintln!("{SENTINEL}: fixture Git is offline");
        process::exit(91);
    }
    let is_fetch = command == "fetch";
    if command == "init" {
        require(
            args.iter().any(|arg| arg == "--bare"),
            "repository is not bare",
        );
        require(
            args.iter().any(|arg| arg == "--template="),
            "templates not disabled",
        );
        require(
            args.last() == Some(&repository),
            "unexpected init directory",
        );
    }
    if is_fetch {
        for flag in [
            "--depth=1",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "--",
        ] {
            require(
                args.iter().any(|arg| arg == flag),
                "fetch isolation flag missing",
            );
        }
        let source = args.len() - 2;
        require(
            args[source - 1] == "--",
            "fetch URL is not after option separator",
        );
        require(
            args[source + 1].len() == 40
                && args[source + 1]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "fetch revision is not a full OID",
        );
        if args[source].starts_with("https://fixture.invalid/failure/") {
            println!("{SENTINEL}: echoed server response");
            eprintln!("{SENTINEL}: echoed remote failure");
            process::exit(92);
        }
        require(
            args[source] == "https://fixture.invalid/seed",
            "unexpected fixture source",
        );
        args[source] = git_path(&root.join("remote"));
    }
    let mut git = Command::new(GIT);
    git.args(args);
    if is_fetch {
        // Only this explicitly trusted fixture substitutes a local transport.
        git.env("GIT_ALLOW_PROTOCOL", "file");
    }
    process::exit(git.status().unwrap().code().unwrap_or(93));
}
