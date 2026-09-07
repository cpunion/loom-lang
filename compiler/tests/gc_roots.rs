use std::{fs, process::Command};
mod common;
use common::success;

#[test]
fn roots_preserve_snapshots_and_managed_control_flow_before_and_after_inlining() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.loom"),
        r#"
import std.text.concat
import std.list.new
import std.list.push
import std.list.get

record Packet { label Text values List[Text] }
enum Choice { First(Packet) Second(Packet) }

fn packet(label Text) Packet {
    let values = new[Text]()
    push(values, concat(label, "-item"))
    Packet { label = concat(label, "-label") values = values }
}
fn choose(side Bool, early Bool) Choice {
    if early { return Choice.First(packet("early")) }
    if side { Choice.First(packet("left")) } else { Choice.Second(packet("right")) }
}
fn describe(choice Choice) Text {
    match choice {
        Choice.First(value) => {
            discard concat("collect", "-first")
            concat(value.label, get(value.values, 0))
        }
        Choice.Second(value) => {
            discard concat("collect", "-second")
            concat(value.label, get(value.values, 0))
        }
    }
}
fn join(first Text, second Text) Text { concat(first, second) }
fn main() {
    var saved = concat("or", "iginal")
    discard concat("replace", "-old-temporaries")
    let combined = join(saved, {
        saved = concat("re", "placed")
        discard concat("collect", "-after-reassignment")
        concat("!", "")
    })
    assert combined == "original!" && saved == "replaced"

    let fields = Packet {
        label = concat("fi", "rst")
        values = {
            discard concat("collect", "-after-first-field")
            let values = new[Text]()
            push(values, concat("se", "cond"))
            values
        }
    }
    assert fields.label == "first" && get(fields.values, 0) == "second"
    var index = 0
    while index < 6 {
        let choice = choose(index % 2 == 0, index == 2)
        discard concat("collect", "-after-choice")
        let text = describe(choice)
        let expected = if index == 2 { "early-labelearly-item" } else {
            if index % 2 == 0 { "left-labelleft-item" } else { "right-labelright-item" }
        }
        assert text == expected
        index = index + 1
    }
}
"#,
    )
    .unwrap();
    let executable = common::executable(directory.path(), "roots");
    let ir = directory.path().join("roots.ll");
    for optimization in ["0", "3"] {
        success(
            &common::command(&[
                "build",
                directory.path().to_str().unwrap(),
                "--output",
                executable.to_str().unwrap(),
                "--emit-ir",
                ir.to_str().unwrap(),
            ])
            .env("LOOM_OPT_LEVEL", optimization)
            .output()
            .unwrap(),
        );
        success(
            &Command::new(&executable)
                .env("LOOM_GC_STRESS", "1")
                .output()
                .unwrap(),
        );
        let ir = fs::read_to_string(&ir).unwrap();
        assert!(!ir.contains("call void @loom.gc."), "unlowered GC region");
        for function in ir.split("define ").skip(1) {
            let body = function.split("\n}").next().unwrap();
            assert!(
                body.matches("call void @loom_rt_roots_enter").count() <= 1,
                "one physical root frame per final native function"
            );
        }
    }
}
