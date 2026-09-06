//! Directory packages and their explicit local import closure.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{Diagnostic, PackageFile, Source};
use crate::parser;

pub struct Loaded {
    pub sources: Vec<Source>,
    pub files: Vec<PackageFile>,
    pub root_package: String,
}

struct Loader {
    loaded: Loaded,
    module: Option<(String, PathBuf)>,
    std_root: PathBuf,
    active: BTreeSet<String>,
    complete: BTreeSet<String>,
}

pub fn diagnostic(error: &Diagnostic, sources: &[Source]) -> String {
    let Some(source) = sources.get(error.span.source) else {
        return error.message.clone();
    };
    let prefix = source.text.get(..error.span.start).unwrap_or("");
    let line = prefix.bytes().filter(|&byte| byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!(
        "{}:{line}:{column}: {}",
        source.path.display(),
        error.message
    )
}

/// Load one package. Only its tests are selected, never dependency tests.
pub fn load(path: &Path, tests: bool) -> Result<Loaded, String> {
    let directory = path
        .canonicalize()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !directory.is_dir() {
        return Err("expected a package directory, not an individual source file".into());
    }
    let module = directory
        .ancestors()
        .find(|dir| dir.join("loom.toml").is_file())
        .map(read_module)
        .transpose()?;
    let std_root = std::env::var_os("LOOM_STD").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std"),
        PathBuf::from,
    );
    let std_root = std_root
        .canonicalize()
        .map_err(|error| format!("standard library: {error}"))?;
    let root_package = if let Some((name, base)) = &module {
        let mut parts = vec![name.clone()];
        for part in directory
            .strip_prefix(base)
            .map_err(|error| error.to_string())?
            .iter()
        {
            let part = part.to_str().ok_or("package paths must be UTF-8")?;
            if !package_segment(part) {
                return Err(format!("invalid package directory {part}"));
            }
            parts.push(part.into());
        }
        parts.join(".")
    } else if let Ok(relative) = directory.strip_prefix(&std_root) {
        std::iter::once("std".into())
            .chain(
                relative
                    .iter()
                    .map(|part| part.to_string_lossy().into_owned()),
            )
            .collect::<Vec<String>>()
            .join(".")
    } else {
        String::new()
    };
    let mut loader = Loader {
        loaded: Loaded {
            sources: vec![],
            files: vec![],
            root_package: root_package.clone(),
        },
        module,
        std_root,
        active: BTreeSet::new(),
        complete: BTreeSet::new(),
    };
    loader.visit(&root_package, &directory, tests)?;
    Ok(loader.loaded)
}

fn package_segment(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn read_module(directory: &Path) -> Result<(String, PathBuf), String> {
    let path = directory.join("loom.toml");
    let source =
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let table = source
        .parse::<toml::Table>()
        .map_err(|_| format!("{}: invalid TOML manifest", path.display()))?;
    for key in table.keys() {
        if !matches!(
            key.as_str(),
            "schema" | "language" | "module" | "dependencies"
        ) {
            return Err(format!(
                "{}: manifest field {key} is not supported by the native seed",
                path.display()
            ));
        }
    }
    if table
        .get("schema")
        .is_some_and(|value| value.as_integer() != Some(2))
    {
        return Err(format!("{}: expected manifest schema 2", path.display()));
    }
    if table
        .get("language")
        .is_some_and(|value| value.as_str() != Some("0.4"))
    {
        return Err(format!(
            "{}: the seed accepts the existing 0.4 source spelling only",
            path.display()
        ));
    }
    if let Some(dependencies) = table.get("dependencies") {
        if dependencies
            .as_table()
            .is_none_or(|table| !table.is_empty())
        {
            return Err(format!(
                "{}: dependency resolution is not implemented in the native seed",
                path.display()
            ));
        }
    }
    let module = table
        .get("module")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| format!("{}: missing [module]", path.display()))?;
    let name = module
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or("missing module.name")?;
    if !package_segment(name) || name == "std" {
        return Err("module.name must be a lowercase package identifier other than std".into());
    }
    Ok((name.into(), directory.to_path_buf()))
}

impl Loader {
    fn visit(&mut self, package: &str, directory: &Path, tests: bool) -> Result<(), String> {
        if self.active.contains(package) {
            return Err(format!("package import cycle through {package}"));
        }
        if self.complete.contains(package) {
            return Ok(());
        }
        self.active.insert(package.into());
        let mut paths = fs::read_dir(directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        paths.sort();
        let mut imports = BTreeSet::new();
        let mut count = 0;
        for path in paths {
            if !path.is_file() || path.extension().is_none_or(|extension| extension != "loom") {
                continue;
            }
            let test_only = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with("_test.loom"));
            if test_only && !tests {
                continue;
            }
            count += 1;
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let source_id = self.loaded.sources.len();
            self.loaded.sources.push(Source { path, text });
            let mut syntax = parser::parse(source_id, &self.loaded.sources[source_id].text)
                .map_err(|error| diagnostic(&error, &self.loaded.sources))?;
            if !tests {
                syntax.functions.retain(|function| !function.test);
            }
            if syntax
                .data
                .iter()
                .any(|data| matches!(data.kind, crate::model::ast::DataKind::Refined { .. }))
            {
                // Checked construction returns these ordinary source-defined
                // language items; their identity and shape are checked later.
                imports.insert("std.result".into());
            }
            for import in &syntax.imports {
                if import.path.len() < 2 {
                    return Err(diagnostic(
                        &Diagnostic::new(
                            import.span,
                            "imports name a module/package and declaration",
                        ),
                        &self.loaded.sources,
                    ));
                }
                let target = import.path[..import.path.len() - 1].join(".");
                if target != package {
                    imports.insert(target);
                }
            }
            self.loaded.files.push(PackageFile {
                package: package.into(),
                trusted_std: self.loaded.sources[source_id]
                    .path
                    .canonicalize()
                    .is_ok_and(|path| path.starts_with(&self.std_root)),
                test_only,
                syntax,
            });
        }
        if count == 0 {
            return Err(format!(
                "{}: no {}Loom source files",
                directory.display(),
                if tests { "" } else { "production " }
            ));
        }
        for import in imports {
            let path = self.import_directory(&import)?;
            self.visit(&import, &path, false)?;
        }
        self.active.remove(package);
        self.complete.insert(package.into());
        Ok(())
    }

    fn import_directory(&self, package: &str) -> Result<PathBuf, String> {
        let mut parts = package.split('.');
        let name = parts.next().ok_or("empty package import")?;
        let mut directory = if name == "std" {
            self.std_root.clone()
        } else if let Some((module, root)) = &self.module {
            if name != module {
                return Err(format!(
                    "unknown module {name}; dependency resolution is not implemented in the seed"
                ));
            }
            root.clone()
        } else {
            return Err(format!(
                "import {package} needs a loom.toml module (only std is available without one)"
            ));
        };
        for part in parts {
            if !package_segment(part) {
                return Err(format!("invalid package path {package}"));
            }
            directory.push(part);
            if directory.join("loom.toml").is_file() {
                return Err(format!("import {package} crosses a nested module boundary"));
            }
        }
        Ok(directory)
    }
}
