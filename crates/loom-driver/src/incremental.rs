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

pub(crate) fn module_interfaces(
    sources: &SourceMap,
    parses: &BTreeMap<FileId, Parse>,
) -> Vec<ModuleInterface> {
    let mut modules = BTreeMap::<String, Vec<(String, serde_json::Value)>>::new();
    for (file, parse) in parses {
        let ast = parse.ast();
        let module = ast.module.as_ref().map_or_else(
            || format!("<missing:{}>", file.0),
            |declaration| declaration.name.as_string(),
        );
        let imports = serde_json::to_value(&ast.imports).unwrap_or(serde_json::Value::Null);
        let declarations = ast
            .declarations
            .iter()
            .filter_map(project_declaration)
            .collect::<Vec<_>>();
        let mut surface = serde_json::json!({
            "imports": imports,
            "declarations": declarations,
        });
        erase_source_ranges(&mut surface);
        let path = sources.document(*file).map_or_else(
            || format!("file-{}", file.0),
            |source| source.relative_path().to_owned(),
        );
        modules.entry(module).or_default().push((path, surface));
    }

    modules
        .into_iter()
        .map(|(module, mut files)| {
            files.sort_by(|left, right| left.0.cmp(&right.0));
            let paths = files.iter().map(|(path, _)| path.clone()).collect();
            let wire = serde_json::to_vec(&("loom-module-interface-v1", &module, &files))
                .expect("module interface JSON values are serializable");
            ModuleInterface {
                module,
                files: paths,
                fingerprint: format!("{:x}", Sha256::digest(wire)),
            }
        })
        .collect()
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
