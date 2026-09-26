use std::fs;

mod common;
use common::{loom, success};

#[test]
fn independent_reachable_functions_do_not_share_a_specialization_budget() {
    let package = tempfile::tempdir().unwrap();
    let declarations = (0..1100)
        .map(|index| format!("fn value_{index}() Int {{ {index} }}\n"))
        .collect::<String>();
    let calls = (0..1100)
        .map(|index| format!("discard value_{index}()\n"))
        .collect::<String>();
    fs::write(
        package.path().join("main.loom"),
        format!("{declarations}fn main() {{\n{calls}}}\n"),
    )
    .unwrap();
    success(&loom(&["check", package.path().to_str().unwrap()]));
}

#[test]
fn one_functions_specialization_budget_reports_the_cross_file_callee_declaration() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("identity.loom"),
        "fn identity[T](value T) T { value }\n",
    )
    .unwrap();
    let declarations = (0..1025)
        .map(|index| format!("record Item_{index} {{}}\n"))
        .collect::<String>();
    let calls = (0..1025)
        .map(|index| format!("discard identity(Item_{index} {{}})\n"))
        .collect::<String>();
    // These caller spans lie beyond identity.loom's EOF. The diagnostic must
    // pair the callee's file with its declaration span, not the caller's span.
    fs::write(
        package.path().join("main.loom"),
        format!("{declarations}fn main() {{\n{calls}}}\n"),
    )
    .unwrap();
    let output = loom(&["check", package.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("identity.loom:1:"), "{diagnostic}");
    assert!(
        diagnostic.contains("function specialization budget exceeded; expansion must be finite"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("assertion failed"), "{diagnostic}");
}

#[test]
fn unbounded_static_recursion_still_renders_a_diagnostic() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("grow.loom"),
        "fn grow(comptime count Int) Int { next(count + 1) }\n",
    )
    .unwrap();
    // Growing static recursion reaches the separate staging-depth guard before
    // it can fill the specialization budget. Its failure must also render.
    fs::write(
        package.path().join("main.loom"),
        format!(
            "// {}\nfn next(comptime count Int) Int {{ grow(count) }}\nfn main() {{ discard grow(0) }}\n",
            "caller padding ".repeat(30),
        ),
    )
    .unwrap();
    let output = loom(&["check", package.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("main.loom:2:"), "{diagnostic}");
    assert!(
        diagnostic.contains("compile-time staging depth exceeded; check for a dependency cycle"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("assertion failed"), "{diagnostic}");
}

#[test]
fn growing_pack_specialization_reports_depth_without_crashing() {
    let package = tempfile::tempdir().unwrap();
    fs::write(
        package.path().join("main.loom"),
        "fn growing[Ts...](values Ts...) { growing(values..., 1) }\nfn main() { growing() }\n",
    )
    .unwrap();
    let output = loom(&["check", package.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("main.loom:1:"), "{diagnostic}");
    assert!(
        diagnostic.contains("type pack specialization depth exceeded; expansion must be finite"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("assertion failed"), "{diagnostic}");
}

#[test]
fn finite_pack_specialization_chain_can_exceed_the_old_native_stack_guard() {
    let package = tempfile::tempdir().unwrap();
    let mut source = String::new();
    for index in 0..48 {
        source.push_str(&format!(
            "fn step_{index}[Ts...](values Ts...) {{ step_{}(values..., 1) }}\n",
            index + 1
        ));
    }
    source.push_str("fn step_48[Ts...](values Ts...) { discard values }\n");
    source.push_str("fn main() { step_0() }\n");
    fs::write(package.path().join("main.loom"), source).unwrap();
    success(&loom(&["check", package.path().to_str().unwrap()]));
}

#[test]
fn deferred_pack_callees_are_checked_before_the_package_succeeds() {
    let package = tempfile::tempdir().unwrap();
    let mut source = String::new();
    for index in 0..48 {
        source.push_str(&format!(
            "fn step_{index}[Ts...](values Ts...) {{ step_{}(values..., 1) }}\n",
            index + 1
        ));
    }
    source.push_str("fn step_48[Ts...](values Ts...) { discard missing }\n");
    source.push_str("fn main() { step_0() }\n");
    fs::write(package.path().join("main.loom"), source).unwrap();
    let output = loom(&["check", package.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("unknown local value"), "{diagnostic}");
    assert!(!diagnostic.contains("type pack specialization depth exceeded"), "{diagnostic}");
}
