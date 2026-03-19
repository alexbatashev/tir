#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use logos::Logos;

    use crate::lexer::Token;
    use crate::preprocessor::preprocessed;

    fn pp(source: &str) -> Vec<Token> {
        preprocessed(source.as_bytes(), HashMap::new(), &[]).collect()
    }

    /// Helper: accepts `("NAME", "body")` pairs, lexes each body to a Token
    /// (same as how `#define` would parse it at runtime).
    fn pp_with_defines(source: &str, defines: &[(&str, &str)]) -> Vec<Token> {
        let defines = defines
            .iter()
            .map(|(k, v)| {
                let tok = Token::lexer(v.trim())
                    .next()
                    .and_then(|r: Result<Token, _>| r.ok())
                    .unwrap_or(Token::Hash);
                (k.to_string(), tok)
            })
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
        insta::assert_debug_snapshot!(pp("#define MYINT int\nMYINT x;"));
    }

    #[test]
    fn test_define_no_body() {
        // A macro with no body should expand to nothing
        insta::assert_debug_snapshot!(pp("#define FLAG\nint FLAG;"));
    }

    #[test]
    fn test_predefined_macro() {
        insta::assert_debug_snapshot!(pp_with_defines("int x = N;", &[("N", "7")]));
    }

    // ── #undef ───────────────────────────────────────────────────────────

    #[test]
    fn test_undef() {
        insta::assert_debug_snapshot!(pp("#define FOO 1\n#undef FOO\nint x = FOO;"));
    }

    // ── #ifdef / #ifndef ─────────────────────────────────────────────────

    #[test]
    fn test_ifdef_defined() {
        insta::assert_debug_snapshot!(pp("#define X\n#ifdef X\nint a;\n#endif\nint b;"));
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
        insta::assert_debug_snapshot!(pp("#define X\n#ifdef X\nint a;\n#else\nint b;\n#endif"));
    }

    #[test]
    fn test_ifdef_else_not_taken() {
        insta::assert_debug_snapshot!(pp("#ifdef X\nint a;\n#else\nint b;\n#endif"));
    }

    // ── nested conditionals ──────────────────────────────────────────────

    #[test]
    fn test_nested_ifdef_outer_false() {
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

    // ── #if expression evaluation ─────────────────────────────────────────

    #[test]
    fn test_if_true() {
        insta::assert_debug_snapshot!(pp("#if 1\nint a;\n#endif\nint b;"));
    }

    #[test]
    fn test_if_false() {
        insta::assert_debug_snapshot!(pp("#if 0\nint a;\n#endif\nint b;"));
    }

    #[test]
    fn test_if_expr() {
        insta::assert_debug_snapshot!(pp("#if 2 + 3 > 4\nint a;\n#endif"));
    }

    #[test]
    fn test_if_macro_value() {
        insta::assert_debug_snapshot!(pp("#define LEVEL 3\n#if LEVEL > 2\nint a;\n#endif"));
    }

    #[test]
    fn test_if_elif_taken() {
        insta::assert_debug_snapshot!(pp("#if 0\nint a;\n#elif 1\nint b;\n#endif"));
    }

    #[test]
    fn test_if_elif_not_taken() {
        insta::assert_debug_snapshot!(pp("#if 1\nint a;\n#elif 1\nint b;\n#endif"));
    }

    #[test]
    fn test_if_defined() {
        insta::assert_debug_snapshot!(pp(
            "#define X\n#if defined(X)\nint a;\n#endif\n#if defined(Y)\nint b;\n#endif\nint c;"
        ));
    }
}
