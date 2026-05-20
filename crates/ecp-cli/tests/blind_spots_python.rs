//! Integration test: Python blind-spot detection.
use ecp_analyzer::python::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;

#[test]
fn python_eval_exec_compile_blind_spots() {
    let src = include_str!("fixtures/blind_spots.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    let kinds: Vec<&str> = local
        .blind_spots
        .iter()
        .map(|bs| bs.kind.as_str())
        .collect();

    assert!(
        kinds.contains(&"python-eval"),
        "missing python-eval, got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"python-exec"),
        "missing python-exec, got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"python-compile"),
        "missing python-compile, got: {:?}",
        kinds
    );
}

#[test]
fn python_dynamic_import_blind_spots() {
    let src = include_str!("fixtures/blind_spots.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    let kinds: Vec<&str> = local
        .blind_spots
        .iter()
        .map(|bs| bs.kind.as_str())
        .collect();

    assert!(
        kinds.contains(&"python-dynamic-import"),
        "missing python-dynamic-import (importlib.import_module), got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"python-builtin-import"),
        "missing python-builtin-import (__import__), got: {:?}",
        kinds
    );
}

#[test]
fn python_cross_getattr_blind_spot_but_self_is_not() {
    let src = include_str!("fixtures/blind_spots.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    let cross: Vec<_> = local
        .blind_spots
        .iter()
        .filter(|bs| bs.kind == "python-cross-getattr")
        .collect();

    // Cross-object getattr(other, name)() SHOULD be a blind spot.
    assert_eq!(
        cross.len(),
        1,
        "expected 1 cross-getattr blind spot, got {}: {:?}",
        cross.len(),
        local.blind_spots
    );
}

#[test]
fn normal_python_code_emits_zero_blind_spots() {
    let src = "def normal_function():\n    return 42\n";
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("normal.py".as_ref(), src.as_bytes())
        .unwrap();

    assert_eq!(
        local.blind_spots.len(),
        0,
        "normal code should emit 0 blind spots, got {:?}",
        local.blind_spots
    );
}

#[test]
fn blind_spot_hints_are_llm_readable() {
    let src = include_str!("fixtures/blind_spots.py");
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();

    for bs in &local.blind_spots {
        assert!(!bs.hint.is_empty(), "hint must not be empty: {:?}", bs);
        assert!(bs.hint.len() > 20, "hint too short to be helpful: {:?}", bs);
        // Hint must reference the pattern type semantically.
        assert!(
            bs.hint.contains(" — ") || bs.hint.contains(": "),
            "hint should have ` — ` or `: ` separator: {:?}",
            bs
        );
    }
}
