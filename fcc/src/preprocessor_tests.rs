#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::preprocessor::preprocessed;
    use crate::lexer::Token;

    fn pp(source: &str) -> Vec<Token> {
        preprocessed(source.as_bytes(), HashMap::new(), &[]).collect()
    }

    fn pp_with_defines(source: &str, defines: &[(&str, &str)]) -> Vec<Token> {
        let defines = defines
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        preprocessed(source.as_bytes(), defines, &[]).collect()
    }

    // ── plain C passthrough ──────────────────────────────────────────────

    #[test]
    fn test_passthrough() {
        insta::assert_debug_snapshot!(pp("int x;"));
    }

    // ── #define / object-macro expansion ────────────────────────────────

    #[test]
    fn test_define_integer() {
        insta::assert_debug_snapshot!(pp("#define LIMIT 42\nint x = LIMIT;"));
    }

    #[test]
    fn test_define_keyword() {
        // A macro that expands to a keyword
        insta::assert_debug_snapshot!(pp("#define MYINT int\nMYINT x;"));
    }

    #[test]
    fn test_define_no_body() {
        // A macro with no body should expand to nothing
        insta::assert_debug_snapshot!(pp("#define FLAG\nint FLAG;"));
    }

    #[test]
    fn test_predefined_macro() {
        // Macros supplied as -D flags via the `defines` argument
        insta::assert_debug_snapshot!(pp_with_defines("int x = N;", &[("N", "7")]));
    }

    // ── #undef ───────────────────────────────────────────────────────────

    #[test]
    fn test_undef() {
        // After #undef the name should be emitted as a plain identifier
        insta::assert_debug_snapshot!(pp("#define FOO 1\n#undef FOO\nint x = FOO;"));
    }

    // ── #ifdef / #ifndef ─────────────────────────────────────────────────

    #[test]
    fn test_ifdef_defined() {
        insta::assert_debug_snapshot!(pp(
            "#define X\n#ifdef X\nint a;\n#endif\nint b;"
        ));
    }

    #[test]
    fn test_ifdef_not_defined() {
        insta::assert_debug_snapshot!(pp("#ifdef MISSING\nint a;\n#endif\nint b;"));
    }

    #[test]
    fn test_ifndef_defined() {
        insta::assert_debug_snapshot!(pp("#define X\n#ifndef X\nint a;\n#endif\nint b;"));
    }

    #[test]
    fn test_ifndef_not_defined() {
        insta::assert_debug_snapshot!(pp("#ifndef MISSING\nint a;\n#endif\nint b;"));
    }

    // ── #else ────────────────────────────────────────────────────────────

    #[test]
    fn test_ifdef_else_taken() {
        // X is defined → first branch active, else branch skipped
        insta::assert_debug_snapshot!(pp(
            "#define X\n#ifdef X\nint a;\n#else\nint b;\n#endif"
        ));
    }

    #[test]
    fn test_ifdef_else_not_taken() {
        // X not defined → first branch skipped, else branch active
        insta::assert_debug_snapshot!(pp("#ifdef X\nint a;\n#else\nint b;\n#endif"));
    }

    // ── nested conditionals ──────────────────────────────────────────────

    #[test]
    fn test_nested_ifdef_outer_false() {
        // Outer condition false → inner block irrelevant; only `int c;` emitted
        insta::assert_debug_snapshot!(pp(
            "#ifdef OUTER\n#ifdef INNER\nint a;\n#else\nint b;\n#endif\n#endif\nint c;"
        ));
    }

    #[test]
    fn test_nested_ifdef_outer_true_inner_false() {
        insta::assert_debug_snapshot!(pp(
            "#define OUTER\n#ifdef OUTER\n#ifdef INNER\nint a;\n#else\nint b;\n#endif\n#endif\nint c;"
        ));
    }

    // ── #if (expression eval not implemented → treated as false) ─────────

    #[test]
    fn test_if_skipped() {
        insta::assert_debug_snapshot!(pp("#if 1\nint a;\n#endif\nint b;"));
    }
}
