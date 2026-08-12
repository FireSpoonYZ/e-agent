//! Compile-fail coverage for the extension and state validation rules.

#[test]
fn rejects_invalid_extensions() {
    trybuild::TestCases::new().compile_fail("tests/compile-fail/*.rs");
}
