use std::process::Command;

use loom_syntax::{DeclKind, MAX_SYNTAX_NESTING, Parse, SYNTAX_NESTING_LIMIT_VERSION, parse};

const CHILD_ENV: &str = "LOOM_SYNTAX_DEEP_INPUT_CHILD";
const ADVERSARIAL_DEPTH: usize = 20_000;

fn has_code(parsed: &Parse, code: &str) -> bool {
    parsed
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.code == code)
}

fn has_good_declaration(parsed: &Parse) -> bool {
    parsed.ast().declarations.iter().any(|declaration| {
        matches!(
            &declaration.kind,
            DeclKind::Record(record) if record.name.text == "Good"
        ) || matches!(
            &declaration.kind,
            DeclKind::Function(function) if function.signature.name.text == "good"
        )
    })
}

fn assert_limited_and_recovered(source: &str) {
    let parsed = parse(source);
    assert!(
        has_code(&parsed, "SyntaxNestingLimit"),
        "missing nesting diagnostic; got {:?}",
        parsed
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
    );
    assert!(has_good_declaration(&parsed));
    assert_eq!(parsed.reconstructed(), source);
}

fn assert_at_limit_and_recovered(source: &str) {
    let parsed = parse(source);
    assert!(!has_code(&parsed, "SyntaxNestingLimit"));
    assert!(has_good_declaration(&parsed));
    assert_eq!(parsed.reconstructed(), source);
}

#[test]
fn nesting_contract_is_versioned_and_has_an_exact_boundary() {
    assert_eq!(SYNTAX_NESTING_LIMIT_VERSION, 2);

    let at_limit = format!(
        "module boundary\ntype Deep = Bool where {}true\nrecord Good {{}}\n",
        "-".repeat(MAX_SYNTAX_NESTING)
    );
    assert_at_limit_and_recovered(&at_limit);

    let beyond_limit = format!(
        "module boundary\ntype Deep = Bool where {}true\nrecord Good {{}}\n",
        "-".repeat(MAX_SYNTAX_NESTING + 1)
    );
    assert_limited_and_recovered(&beyond_limit);

    let parentheses_at = format!(
        "module boundary\ntype Deep = Bool where {}true{}\nrecord Good {{}}\n",
        "(".repeat(MAX_SYNTAX_NESTING),
        ")".repeat(MAX_SYNTAX_NESTING)
    );
    assert_at_limit_and_recovered(&parentheses_at);
    let parentheses_beyond = format!(
        "module boundary\ntype Deep = Bool where {}true{}\nrecord Good {{}}\n",
        "(".repeat(MAX_SYNTAX_NESTING + 1),
        ")".repeat(MAX_SYNTAX_NESTING + 1)
    );
    assert_limited_and_recovered(&parentheses_beyond);

    let generic_at = format!(
        "module boundary\nrecord Deep {{ value {}Int{} }}\nrecord Good {{}}\n",
        "Wrap[".repeat(MAX_SYNTAX_NESTING),
        "]".repeat(MAX_SYNTAX_NESTING)
    );
    assert_at_limit_and_recovered(&generic_at);
    let generic_beyond = format!(
        "module boundary\nrecord Deep {{ value {}Int{} }}\nrecord Good {{}}\n",
        "Wrap[".repeat(MAX_SYNTAX_NESTING + 1),
        "]".repeat(MAX_SYNTAX_NESTING + 1)
    );
    assert_limited_and_recovered(&generic_beyond);

    let member_at = format!(
        "module boundary\ntype Deep = Bool where root{}\nrecord Good {{}}\n",
        ".field".repeat(MAX_SYNTAX_NESTING)
    );
    assert_at_limit_and_recovered(&member_at);
    let member_beyond = format!(
        "module boundary\ntype Deep = Bool where root{}\nrecord Good {{}}\n",
        ".field".repeat(MAX_SYNTAX_NESTING + 1)
    );
    assert_limited_and_recovered(&member_beyond);

    // The enclosing match consumes one level before its pattern is parsed.
    let pattern_limit = MAX_SYNTAX_NESTING - 1;
    let pattern_at = format!(
        "module boundary\ntype Deep = Bool where match value {{ {}_{} => true }}\nrecord Good {{}}\n",
        "Some(".repeat(pattern_limit),
        ")".repeat(pattern_limit)
    );
    assert_at_limit_and_recovered(&pattern_at);
    let pattern_beyond = format!(
        "module boundary\ntype Deep = Bool where match value {{ {}_{} => true }}\nrecord Good {{}}\n",
        "Some(".repeat(pattern_limit + 1),
        ")".repeat(pattern_limit + 1)
    );
    assert_limited_and_recovered(&pattern_beyond);
}

#[test]
fn deep_inputs_do_not_overflow_the_process_stack() {
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "deep_inputs_child", "--nocapture"])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn nesting regression child");

    assert!(
        output.status.success(),
        "child failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("test deep_inputs_child ... ok"),
        "child test filter did not execute the intended test: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn deep_inputs_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }

    let unary = format!(
        "module unary\ntype Deep = Bool where {}true\nrecord Good {{}}\n",
        "-".repeat(ADVERSARIAL_DEPTH)
    );
    assert_limited_and_recovered(&unary);

    let binary = format!(
        "module binary\ntype Deep = Bool where true{}\nrecord Good {{}}\n",
        " || true".repeat(ADVERSARIAL_DEPTH)
    );
    assert_limited_and_recovered(&binary);

    let parentheses = format!(
        "module parens\ntype Deep = Bool where {}true{}\nrecord Good {{}}\n",
        "(".repeat(ADVERSARIAL_DEPTH),
        ")".repeat(ADVERSARIAL_DEPTH)
    );
    assert_limited_and_recovered(&parentheses);

    let generic_type = format!(
        "module generic\nrecord Deep {{ value {}Int{} }}\nrecord Good {{}}\n",
        "Wrap[".repeat(ADVERSARIAL_DEPTH),
        "]".repeat(ADVERSARIAL_DEPTH)
    );
    assert_limited_and_recovered(&generic_type);

    let qualified_projection = format!(
        "module projection\nrecord Deep {{ value {}T{} }}\nrecord Good {{}}\n",
        "<".repeat(ADVERSARIAL_DEPTH),
        " as C>.Item".repeat(ADVERSARIAL_DEPTH)
    );
    assert_limited_and_recovered(&qualified_projection);

    let member_projection = format!(
        "module member\ntype Deep = Bool where root{}\nrecord Good {{}}\n",
        ".field".repeat(ADVERSARIAL_DEPTH)
    );
    assert_limited_and_recovered(&member_projection);

    let pattern = format!(
        "module pattern\nfn deep(value T) Unit {{ match value {{ {}_{} => Unit }} }}\nfn good() Unit {{ Unit }}\n",
        "Some(".repeat(ADVERSARIAL_DEPTH),
        ")".repeat(ADVERSARIAL_DEPTH)
    );
    assert_limited_and_recovered(&pattern);
}
