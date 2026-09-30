//! Trusted test executable: checks Loom's process boundary, then forwards to real
//! Git. Only the fixture HTTPS URL is remapped; object storage and plumbing stay
//! Git's responsibility. Constants survive the resolver's cleared environment.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{Read, Write},
    path::Path,
    process::{self, Command, Stdio},
};

const ROOT: &str = env!("LOOM_GIT_FIXTURE_ROOT");
const GIT: &str = env!("LOOM_GIT_FIXTURE_REAL_GIT");
const SENTINEL: &str = "LOOM_GIT_REMOTE_SENTINEL_secret";
const AUTH_HEADER: &str = "Authorization: Basic bG9vbTpMT09NX0dJVF9SRU1PVEVfU0VOVElORUxfc2VjcmV0";

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
    let https_url = fs::read_to_string(root.join("https-url")).ok();
    let source_url = https_url
        .as_deref()
        .unwrap_or("https://fixture.invalid/seed");
    if env::args().skip(1).collect::<Vec<_>>() == ["get"] {
        let mut request = String::new();
        std::io::stdin().read_to_string(&mut request).unwrap();
        require(
            request
                == format!(
                    "protocol=https\nhost={}\npath=seed\n\n",
                    source_url[8..].split('/').next().unwrap()
                ),
            "credential request scope differs",
        );
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("credential.log"))
            .unwrap()
            .write_all(request.as_bytes())
            .unwrap();
        match fs::read_to_string(root.join("credential-mode"))
            .unwrap_or_default()
            .as_str()
        {
            "fail" => {
                println!("{SENTINEL}");
                eprintln!("{AUTH_HEADER}");
                process::exit(94);
            }
            "scope" => println!("host=elsewhere.invalid\nusername=loom\npassword={SENTINEL}\n"),
            "empty" => {}
            _ => println!("username=loom\npassword={SENTINEL}\n"),
        }
        return;
    }
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
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
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
        args.iter().all(|arg| {
            (!arg.contains(SENTINEL) || arg.starts_with("https://fixture.invalid/failure/"))
                && !arg.contains(AUTH_HEADER)
        }),
        "credential appeared in argv",
    );
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
        ("fetch.uriprotocols", ""),
        ("transfer.bundleURI", "false"),
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
    let authenticated = env::var_os("GIT_CONFIG_COUNT").is_some();
    if authenticated {
        require(
            is_fetch
                && env::var("GIT_CONFIG_COUNT").as_deref() == Ok("1")
                && env::var("GIT_CONFIG_KEY_0").as_deref()
                    == Ok(format!("http.{source_url}.extraHeader").as_str())
                && env::var("GIT_CONFIG_VALUE_0").as_deref() == Ok(AUTH_HEADER),
            "credential escaped its fetch scope",
        );
    } else {
        require(
            env::var_os("GIT_CONFIG_KEY_0").is_none()
                && env::var_os("GIT_CONFIG_VALUE_0").is_none(),
            "partial credential environment",
        );
    }
    if is_fetch && https_url.is_none() && root.join("requires-auth").exists() && !authenticated {
        eprintln!("{SENTINEL}: authentication required");
        process::exit(95);
    }
    if is_fetch && https_url.is_none() && root.join("auth-echo").exists() {
        println!("{AUTH_HEADER}");
        eprintln!("{SENTINEL}: echoed authenticated server response");
        process::exit(96);
    }
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
        require(args[source] == source_url, "unexpected fixture source");
        if https_url.is_none() {
            args[source] = git_path(&root.join("remote"));
        }
    }
    let mut git = Command::new(GIT);
    if https_url.is_some() {
        // Trust only this fixture's ephemeral CA, without changing host trust.
        git.args(["-c", "http.schannelUseSSLCAInfo=true"]);
        if cfg!(windows) {
            // Git applies Schannel options only when the backend is explicitly
            // selected, even when libcurl already defaults to Schannel.
            git.args(["-c", "http.sslBackend=schannel"]);
            // This ephemeral issuer has no CRL/OCSP service. Disable only its
            // revocation lookup, not chain/time/name verification. Production
            // Git arguments above retain the transport's normal verification.
            git.args(["-c", "http.schannelCheckRevoke=false"]);
        }
        git.args([
            "-c",
            &format!("http.sslCAInfo={}", git_path(&root.join("server-ca.pem"))),
        ]);
    }
    git.args(args);
    if is_fetch && https_url.is_none() {
        // Only this explicitly trusted fixture substitutes a local transport.
        git.env("GIT_ALLOW_PROTOCOL", "file");
    }
    let output = git.stdin(Stdio::inherit()).output().unwrap();
    if https_url.is_some() && !output.status.success() {
        // Test-only transport diagnostics, outside Loom's cache and output.
        // Never weaken production redaction to diagnose a fixture handshake.
        let diagnostic = String::from_utf8_lossy(&output.stderr)
            .replace(SENTINEL, "<fixture-secret>")
            .replace(AUTH_HEADER, "<fixture-authorization>");
        fs::write(root.join("transport-error.log"), diagnostic).unwrap();
    }
    std::io::stdout().write_all(&output.stdout).unwrap();
    std::io::stderr().write_all(&output.stderr).unwrap();
    process::exit(output.status.code().unwrap_or(93));
}
