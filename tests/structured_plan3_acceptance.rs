use sparsh_core::{input_completeness, InputCompleteness, ShellResult, ShellSession};
use sparsh_ui::{render_result, Theme};

#[test]
fn plan3_acceptance_structured_preview_renders_and_becomes_underscore() {
    let mut session = ShellSession::new();
    let result = session
        .submit_spar(
            r#"
            import pkg { collectTable } from "std/data";
            struct User { name: str = ""; age: int = 0; };
            [User(name: "Obi", age: 24), User(name: "Ada", age: 31)]
                |> collectTable()
            "#,
        )
        .expect("structured Plan 3 preview should succeed");

    assert!(matches!(result, ShellResult::Structured(_)));
    let mut output = Vec::new();
    render_result(&result, &Theme::plain(), true, &mut output)
        .expect("structured result should render");
    let rendered = String::from_utf8(output).expect("renderer should emit UTF-8");
    assert!(rendered.contains("name"), "{rendered}");
    assert!(rendered.contains("Obi"), "{rendered}");
    assert!(rendered.contains("Ada"), "{rendered}");

    let previous = session
        .submit("_")
        .expect("underscore should recall the structured preview");
    assert!(matches!(previous, ShellResult::Structured(_)));
    let mut previous_output = Vec::new();
    render_result(&previous, &Theme::plain(), true, &mut previous_output)
        .expect("previous structured result should render");
    let previous_rendered = String::from_utf8(previous_output).expect("renderer should emit UTF-8");
    assert!(previous_rendered.contains("Obi"), "{previous_rendered}");
    assert!(previous_rendered.contains("Ada"), "{previous_rendered}");
}

#[test]
fn plan3_acceptance_structured_pipeline_completeness_is_parser_driven() {
    assert_eq!(
        input_completeness("users |>"),
        InputCompleteness::Incomplete
    );
    assert_eq!(
        input_completeness("users |> take(2)"),
        InputCompleteness::Complete
    );
}
