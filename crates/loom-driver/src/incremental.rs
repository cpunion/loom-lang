//! Canonical module interface identities used by incremental dependency keys.

use std::collections::BTreeMap;

use loom_core::{FileId, TextRange};
use loom_syntax::{
    Block, ConformanceMember, Decl, DeclKind, ImplKind, MethodDecl, Parse, Visibility,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::SourceMap;

/// Source-independent public surface of one Loom module.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModuleInterface {
    pub module: String,
    pub files: Vec<String>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModuleQueryKey {
    pub module: String,
    pub interface_fingerprint: String,
    pub shape_fingerprint: String,
    pub body_fingerprint: String,
}

pub(crate) fn module_interfaces(
    sources: &SourceMap,
    parses: &BTreeMap<FileId, Parse>,
) -> Vec<ModuleInterface> {
    module_query_data(sources, parses)
        .into_iter()
        .map(|module| module.interface)
        .collect()
}

pub(crate) fn module_query_keys(
    sources: &SourceMap,
    parses: &BTreeMap<FileId, Parse>,
) -> BTreeMap<String, ModuleQueryKey> {
    module_query_data(sources, parses)
        .into_iter()
        .map(|module| {
            let key = ModuleQueryKey {
                module: module.interface.module.clone(),
                interface_fingerprint: module.interface.fingerprint,
                shape_fingerprint: module.shape_fingerprint,
                body_fingerprint: module.body_fingerprint,
            };
            (key.module.clone(), key)
        })
        .collect()
}

struct ModuleQueryData {
    interface: ModuleInterface,
    shape_fingerprint: String,
    body_fingerprint: String,
}

fn module_query_data(
    sources: &SourceMap,
    parses: &BTreeMap<FileId, Parse>,
) -> Vec<ModuleQueryData> {
    let mut modules = BTreeMap::<
        String,
        Vec<(
            String,
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
        )>,
    >::new();
    for (file, parse) in parses {
        let ast = parse.ast();
        let source_module = ast.module.as_ref().map_or_else(
            || format!("<missing:{}>", file.0),
            |declaration| declaration.name.as_string(),
        );
        let module = sources
            .document(*file)
            .map_or(source_module.clone(), |source| {
                source.package().map_or(source_module.clone(), |package| {
                    format!("{package}::{source_module}")
                })
            });
        let imports = serde_json::to_value(&ast.imports).unwrap_or(serde_json::Value::Null);
        let interface_declarations = ast
            .declarations
            .iter()
            .filter_map(project_declaration)
            .collect::<Vec<_>>();
        let shape_declarations = ast
            .declarations
            .iter()
            .map(project_shape_declaration)
            .collect::<Vec<_>>();
        let mut interface = serde_json::json!({
            "imports": imports,
            "declarations": interface_declarations,
        });
        let mut shape = serde_json::json!({
            "imports": &ast.imports,
            "declarations": shape_declarations,
        });
        let mut body = serde_json::json!({
            "imports": &ast.imports,
            "declarations": &ast.declarations,
        });
        erase_source_ranges(&mut interface);
        erase_source_ranges(&mut shape);
        erase_source_ranges(&mut body);
        let path = sources.document(*file).map_or_else(
            || format!("file-{}", file.0),
            |source| source.relative_path().to_owned(),
        );
        modules
            .entry(module)
            .or_default()
            .push((path, interface, shape, body));
    }

    modules
        .into_iter()
        .map(|(module, mut files)| {
            files.sort_by(|left, right| left.0.cmp(&right.0));
            let paths = files.iter().map(|(path, ..)| path.clone()).collect();
            let interface_files = files
                .iter()
                .map(|(path, interface, _, _)| (path, interface))
                .collect::<Vec<_>>();
            let shape_files = files
                .iter()
                .map(|(path, _, shape, _)| (path, shape))
                .collect::<Vec<_>>();
            let body_files = files
                .iter()
                .map(|(path, _, _, body)| (path, body))
                .collect::<Vec<_>>();
            ModuleQueryData {
                interface: ModuleInterface {
                    module: module.clone(),
                    files: paths,
                    fingerprint: fingerprint("loom-module-interface-v2", &module, &interface_files),
                },
                shape_fingerprint: fingerprint(
                    "loom-module-semantic-shape-v2",
                    &module,
                    &shape_files,
                ),
                body_fingerprint: fingerprint("loom-module-semantic-body-v2", &module, &body_files),
            }
        })
        .collect()
}

fn fingerprint<T: Serialize>(format: &str, module: &str, files: &T) -> String {
    let wire = serde_json::to_vec(&(format, module, files))
        .expect("module query JSON values are serializable");
    format!("{:x}", Sha256::digest(wire))
}

fn project_shape_declaration(declaration: &Decl) -> Decl {
    let mut projected = declaration.clone();
    match &mut projected.kind {
        DeclKind::Function(function) => function.body = empty_block(),
        DeclKind::Impl(implementation) => match &mut implementation.kind {
            ImplKind::Inherent { methods, .. } => {
                for method in methods {
                    strip_method_body(method);
                }
            }
            ImplKind::Conformance { members, .. } => {
                for member in members {
                    if let ConformanceMember::Method(method) = member {
                        strip_method_body(method);
                    }
                }
            }
        },
        DeclKind::ConstrainedType(_)
        | DeclKind::Record(_)
        | DeclKind::Enum(_)
        | DeclKind::Concept(_)
        | DeclKind::Error(_) => {}
    }
    projected
}

fn project_declaration(declaration: &Decl) -> Option<Decl> {
    if matches!(&declaration.kind, DeclKind::Impl(_)) {
        let mut projected = declaration.clone();
        let DeclKind::Impl(implementation) = &mut projected.kind else {
            unreachable!();
        };
        match &mut implementation.kind {
            ImplKind::Inherent { methods, .. } => {
                methods.retain(|method| method.visibility == Visibility::Public);
                for method in methods.iter_mut() {
                    strip_method_body(method);
                }
                if methods.is_empty() {
                    return None;
                }
            }
            ImplKind::Conformance { members, .. } => {
                for member in members {
                    if let ConformanceMember::Method(method) = member {
                        strip_method_body(method);
                    }
                }
            }
        }
        return Some(projected);
    }
    if declaration.visibility != Visibility::Public {
        return None;
    }
    let mut projected = declaration.clone();
    if let DeclKind::Function(function) = &mut projected.kind {
        function.body = empty_block();
    }
    Some(projected)
}

fn strip_method_body(method: &mut MethodDecl) {
    method.body = empty_block();
}

fn empty_block() -> Block {
    Block {
        items: Vec::new(),
        range: TextRange::default(),
    }
}

fn erase_source_ranges(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                erase_source_ranges(value);
            }
        }
        serde_json::Value::Object(fields) => {
            fields.remove("range");
            if fields.len() == 2
                && fields.contains_key("start")
                && fields.contains_key("end")
                && fields.values().all(serde_json::Value::is_u64)
            {
                fields.clear();
                return;
            }
            for value in fields.values_mut() {
                erase_source_ranges(value);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}
