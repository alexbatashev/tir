//! A chumsky parser from SMT-LIB 2.7 concrete syntax to [`crate::smtlib::ast`].
//!
//! The grammar is built from one small parser per production. Each lexical token
//! parser consumes its own *trailing* whitespace and comments; the script parser
//! strips the single leading run. Tokens are matched whole (a keyword check
//! parses an entire symbol token, then compares), so command/operator names with
//! shared prefixes — `check-sat` vs `check-sat-assuming` — need no ordering care.

use chumsky::prelude::*;

use super::ast::*;

type Extra<'a> = extra::Err<Rich<'a, char>>;

/// Whitespace and `;`-to-end-of-line comments, discarded.
fn ws<'a>() -> impl Parser<'a, &'a str, (), Extra<'a>> + Clone {
    let comment = just(';')
        .then(none_of("\n").repeated().collect::<String>())
        .ignored();
    choice((one_of(" \t\r\n").ignored(), comment))
        .repeated()
        .collect::<Vec<_>>()
        .ignored()
}

fn lparen<'a>() -> impl Parser<'a, &'a str, (), Extra<'a>> + Clone {
    just('(').then_ignore(ws()).ignored()
}

fn rparen<'a>() -> impl Parser<'a, &'a str, (), Extra<'a>> + Clone {
    just(')').then_ignore(ws()).ignored()
}

fn symbol_p<'a>() -> impl Parser<'a, &'a str, Symbol, Extra<'a>> + Clone {
    let simple = any()
        .filter(|c: &char| c.is_ascii_alphabetic() || SYMBOL_CHARS.contains(*c))
        .then(
            any()
                .filter(|c: &char| c.is_ascii_alphanumeric() || SYMBOL_CHARS.contains(*c))
                .repeated()
                .collect::<String>(),
        )
        .map(|(first, rest): (char, String)| {
            let mut name = String::with_capacity(rest.len() + 1);
            name.push(first);
            name.push_str(&rest);
            name
        });
    let quoted = just('|')
        .ignore_then(none_of("|\\").repeated().collect::<String>())
        .then_ignore(just('|'));
    choice((simple, quoted)).map(Symbol).then_ignore(ws())
}

/// Match one reserved word, tokenised as a whole symbol to avoid prefix clashes.
fn kw<'a>(word: &'static str) -> impl Parser<'a, &'a str, (), Extra<'a>> + Clone {
    symbol_p().try_map(move |s, span| {
        if s.0 == word {
            Ok(())
        } else {
            Err(Rich::custom(span, format!("expected `{word}`")))
        }
    })
}

/// A `<numeral>` digit run, enforcing the grammar's no-leading-zero rule
/// (`0 | [1-9][0-9]*`). Returns the digits unparsed.
fn numeral_digits<'a>() -> impl Parser<'a, &'a str, String, Extra<'a>> + Clone {
    any()
        .filter(|c: &char| c.is_ascii_digit())
        .repeated()
        .at_least(1)
        .collect::<String>()
        .try_map(|s: String, span| {
            if s.len() > 1 && s.starts_with('0') {
                Err(Rich::custom(span, "numeral must not have a leading zero"))
            } else {
                Ok(s)
            }
        })
}

fn numeral_p<'a>() -> impl Parser<'a, &'a str, u128, Extra<'a>> + Clone {
    numeral_digits()
        .try_map(|s: String, span| {
            s.parse::<u128>()
                .map_err(|_| Rich::custom(span, "numeral out of range"))
        })
        .then_ignore(ws())
}

fn string_lit_p<'a>() -> impl Parser<'a, &'a str, String, Extra<'a>> + Clone {
    just('"')
        .ignore_then(
            choice((just("\"\"").to('"'), none_of("\"")))
                .repeated()
                .collect::<String>(),
        )
        .then_ignore(just('"'))
        .then_ignore(ws())
}

fn spec_constant_p<'a>() -> impl Parser<'a, &'a str, SpecConstant, Extra<'a>> + Clone {
    let digits = || {
        any()
            .filter(|c: &char| c.is_ascii_digit())
            .repeated()
            .at_least(1)
            .collect::<String>()
    };
    let numeral = numeral_digits().try_map(|s: String, span| {
        s.parse::<u128>()
            .map_err(|_| Rich::custom(span, "numeral out of range"))
    });
    let decimal = numeral_digits()
        .then_ignore(just('.'))
        .then(digits())
        .map(|(int, frac): (String, String)| format!("{int}.{frac}"));
    let hex = just("#x").ignore_then(
        any()
            .filter(|c: &char| c.is_ascii_hexdigit())
            .repeated()
            .at_least(1)
            .collect::<String>(),
    );
    let binary = just("#b").ignore_then(one_of("01").repeated().at_least(1).collect::<String>());
    let string = just('"')
        .ignore_then(
            choice((just("\"\"").to('"'), none_of("\"")))
                .repeated()
                .collect::<String>(),
        )
        .then_ignore(just('"'));
    choice((
        hex.map(SpecConstant::Hexadecimal),
        binary.map(SpecConstant::Binary),
        decimal.map(SpecConstant::Decimal),
        numeral.map(SpecConstant::Numeral),
        string.map(SpecConstant::String),
    ))
    .then_ignore(ws())
}

fn keyword_p<'a>() -> impl Parser<'a, &'a str, Keyword, Extra<'a>> + Clone {
    // A keyword is `:` followed by a simple symbol: the first character cannot
    // be a digit.
    let first = any().filter(|c: &char| c.is_ascii_alphabetic() || SYMBOL_CHARS.contains(*c));
    let rest = any()
        .filter(|c: &char| c.is_ascii_alphanumeric() || SYMBOL_CHARS.contains(*c))
        .repeated()
        .collect::<String>();
    just(':')
        .ignore_then(first)
        .then(rest)
        .map(|(first, rest): (char, String)| {
            let mut name = String::with_capacity(rest.len() + 1);
            name.push(first);
            name.push_str(&rest);
            Keyword(name)
        })
        .then_ignore(ws())
}

fn index_p<'a>() -> impl Parser<'a, &'a str, Index, Extra<'a>> + Clone {
    choice((
        numeral_p().map(Index::Numeral),
        symbol_p().map(Index::Symbol),
    ))
}

fn identifier_p<'a>() -> impl Parser<'a, &'a str, Identifier, Extra<'a>> + Clone {
    let plain = symbol_p().map(|symbol| Identifier {
        symbol,
        indices: Vec::new(),
    });
    let indexed = lparen()
        .ignore_then(kw("_"))
        .ignore_then(symbol_p())
        .then(index_p().repeated().at_least(1).collect::<Vec<_>>())
        .then_ignore(rparen())
        .map(|(symbol, indices)| Identifier { symbol, indices });
    choice((indexed, plain))
}

fn sort_p<'a>() -> impl Parser<'a, &'a str, Sort, Extra<'a>> + Clone {
    recursive(|sort| {
        let simple = identifier_p().map(Sort::simple);
        let app = lparen()
            .ignore_then(identifier_p())
            .then(sort.repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(rparen())
            .map(|(id, params)| Sort { id, params });
        // `simple` first so an indexed identifier `(_ BitVec 32)` is read as a
        // sort, not mistaken for a sort application.
        choice((simple, app))
    })
}

fn qual_identifier_p<'a>() -> impl Parser<'a, &'a str, QualIdentifier, Extra<'a>> + Clone {
    let annotated = lparen()
        .ignore_then(kw("as"))
        .ignore_then(identifier_p())
        .then(sort_p())
        .then_ignore(rparen())
        .map(|(id, sort)| QualIdentifier::Annotated(id, sort));
    choice((annotated, identifier_p().map(QualIdentifier::Plain)))
}

fn sorted_var_p<'a>() -> impl Parser<'a, &'a str, SortedVar, Extra<'a>> + Clone {
    lparen()
        .ignore_then(symbol_p())
        .then(sort_p())
        .then_ignore(rparen())
        .map(|(var, sort)| SortedVar { var, sort })
}

fn s_expr_p<'a>() -> impl Parser<'a, &'a str, SExpr, Extra<'a>> + Clone {
    recursive(|s_expr| {
        choice((
            spec_constant_p().map(SExpr::Constant),
            keyword_p().map(SExpr::Keyword),
            symbol_p().map(SExpr::Symbol),
            lparen()
                .ignore_then(s_expr.repeated().collect::<Vec<_>>())
                .then_ignore(rparen())
                .map(SExpr::List),
        ))
    })
}

fn attribute_p<'a>() -> impl Parser<'a, &'a str, Attribute, Extra<'a>> + Clone {
    let value = choice((
        spec_constant_p().map(AttributeValue::Constant),
        symbol_p().map(AttributeValue::Symbol),
        lparen()
            .ignore_then(s_expr_p().repeated().collect::<Vec<_>>())
            .then_ignore(rparen())
            .map(AttributeValue::List),
    ));
    keyword_p()
        .then(value.or_not())
        .map(|(keyword, value)| Attribute { keyword, value })
}

fn term_p<'a>() -> impl Parser<'a, &'a str, Term, Extra<'a>> + Clone {
    recursive(|term| {
        let var_binding = lparen()
            .ignore_then(symbol_p())
            .then(term.clone())
            .then_ignore(rparen())
            .map(|(var, t)| VarBinding { var, term: t });

        let pattern = choice((
            lparen()
                .ignore_then(symbol_p())
                .then(symbol_p().repeated().at_least(1).collect::<Vec<_>>())
                .then_ignore(rparen())
                .map(|(ctor, vars)| Pattern::Constructor(ctor, vars)),
            symbol_p().map(Pattern::Var),
        ));
        let match_case = lparen()
            .ignore_then(pattern)
            .then(term.clone())
            .then_ignore(rparen())
            .map(|(pattern, body)| MatchCase { pattern, body });

        let quantified_vars = lparen()
            .ignore_then(sorted_var_p().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(rparen());

        let let_form = lparen()
            .ignore_then(kw("let"))
            .ignore_then(
                lparen()
                    .ignore_then(var_binding.repeated().at_least(1).collect::<Vec<_>>())
                    .then_ignore(rparen()),
            )
            .then(term.clone())
            .then_ignore(rparen())
            .map(|(binds, body)| Term::Let(binds, Box::new(body)));
        let forall_form = lparen()
            .ignore_then(kw("forall"))
            .ignore_then(quantified_vars.clone())
            .then(term.clone())
            .then_ignore(rparen())
            .map(|(vars, body)| Term::Forall(vars, Box::new(body)));
        let exists_form = lparen()
            .ignore_then(kw("exists"))
            .ignore_then(quantified_vars)
            .then(term.clone())
            .then_ignore(rparen())
            .map(|(vars, body)| Term::Exists(vars, Box::new(body)));
        let match_form = lparen()
            .ignore_then(kw("match"))
            .ignore_then(term.clone())
            .then(
                lparen()
                    .ignore_then(match_case.repeated().at_least(1).collect::<Vec<_>>())
                    .then_ignore(rparen()),
            )
            .then_ignore(rparen())
            .map(|(scrutinee, cases)| Term::Match(Box::new(scrutinee), cases));
        let annot_form = lparen()
            .ignore_then(kw("!"))
            .ignore_then(term.clone())
            .then(attribute_p().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(rparen())
            .map(|(t, attrs)| Term::Annotated(Box::new(t), attrs));
        let app_form = lparen()
            .ignore_then(qual_identifier_p())
            .then(term.repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(rparen())
            .map(|(f, args)| Term::App(f, args));

        let atom = choice((
            spec_constant_p().map(Term::Constant),
            qual_identifier_p().map(Term::Ident),
        ));

        // `atom` before `app_form` so `(_ bv13 8)` and `(as c S)` read as
        // qualified-identifier terms, not as applications headed by `_`/`as`.
        choice((
            let_form,
            forall_form,
            exists_form,
            match_form,
            annot_form,
            atom,
            app_form,
        ))
    })
}

fn function_def_p<'a>() -> impl Parser<'a, &'a str, FunctionDef, Extra<'a>> + Clone {
    symbol_p()
        .then(
            lparen()
                .ignore_then(sorted_var_p().repeated().collect::<Vec<_>>())
                .then_ignore(rparen()),
        )
        .then(sort_p())
        .then(term_p())
        .map(|(((name, params), return_sort), body)| FunctionDef {
            name,
            params,
            return_sort,
            body,
        })
}

fn function_dec_p<'a>() -> impl Parser<'a, &'a str, FunctionDec, Extra<'a>> + Clone {
    lparen()
        .ignore_then(symbol_p())
        .then(
            lparen()
                .ignore_then(sorted_var_p().repeated().collect::<Vec<_>>())
                .then_ignore(rparen()),
        )
        .then(sort_p())
        .then_ignore(rparen())
        .map(|((name, params), return_sort)| FunctionDec {
            name,
            params,
            return_sort,
        })
}

fn prop_literal_p<'a>() -> impl Parser<'a, &'a str, PropLiteral, Extra<'a>> + Clone {
    let negated = lparen()
        .ignore_then(kw("not"))
        .ignore_then(symbol_p())
        .then_ignore(rparen())
        .map(|symbol| PropLiteral {
            symbol,
            negated: true,
        });
    let plain = symbol_p().map(|symbol| PropLiteral {
        symbol,
        negated: false,
    });
    choice((negated, plain))
}

fn command_p<'a>() -> impl Parser<'a, &'a str, Command, Extra<'a>> + Clone {
    let sort_list = || {
        lparen()
            .ignore_then(sort_p().repeated().collect::<Vec<_>>())
            .then_ignore(rparen())
    };

    let group_a = choice((
        kw("set-logic")
            .ignore_then(symbol_p())
            .map(Command::SetLogic),
        kw("set-info")
            .ignore_then(attribute_p())
            .map(Command::SetInfo),
        kw("set-option")
            .ignore_then(attribute_p())
            .map(Command::SetOption),
        kw("declare-sort")
            .ignore_then(symbol_p())
            .then(numeral_p())
            .map(|(name, arity)| Command::DeclareSort(name, arity)),
        kw("define-sort")
            .ignore_then(symbol_p())
            .then(
                lparen()
                    .ignore_then(symbol_p().repeated().collect::<Vec<_>>())
                    .then_ignore(rparen()),
            )
            .then(sort_p())
            .map(|((name, params), def)| Command::DefineSort(name, params, def)),
        kw("declare-const")
            .ignore_then(symbol_p())
            .then(sort_p())
            .map(|(name, sort)| Command::DeclareConst(name, sort)),
        kw("declare-fun")
            .ignore_then(symbol_p())
            .then(sort_list())
            .then(sort_p())
            .map(|((name, args), ret)| Command::DeclareFun(name, args, ret)),
        kw("define-fun-rec")
            .ignore_then(function_def_p())
            .map(Command::DefineFunRec),
        kw("define-funs-rec")
            .ignore_then(
                lparen()
                    .ignore_then(function_dec_p().repeated().at_least(1).collect::<Vec<_>>())
                    .then_ignore(rparen()),
            )
            .then(
                lparen()
                    .ignore_then(term_p().repeated().at_least(1).collect::<Vec<_>>())
                    .then_ignore(rparen()),
            )
            .map(|(decs, bodies)| Command::DefineFunsRec(decs, bodies)),
        kw("define-fun")
            .ignore_then(function_def_p())
            .map(Command::DefineFun),
    ));

    let group_b = choice((
        kw("assert").ignore_then(term_p()).map(Command::Assert),
        kw("check-sat-assuming")
            .ignore_then(
                lparen()
                    .ignore_then(prop_literal_p().repeated().collect::<Vec<_>>())
                    .then_ignore(rparen()),
            )
            .map(Command::CheckSatAssuming),
        kw("check-sat").to(Command::CheckSat),
        kw("get-assertions").to(Command::GetAssertions),
        kw("get-model").to(Command::GetModel),
        kw("get-value")
            .ignore_then(
                lparen()
                    .ignore_then(term_p().repeated().at_least(1).collect::<Vec<_>>())
                    .then_ignore(rparen()),
            )
            .map(Command::GetValue),
        kw("get-proof").to(Command::GetProof),
        kw("get-unsat-core").to(Command::GetUnsatCore),
        kw("get-unsat-assumptions").to(Command::GetUnsatAssumptions),
        kw("get-assignment").to(Command::GetAssignment),
        kw("get-info")
            .ignore_then(keyword_p())
            .map(Command::GetInfo),
        kw("get-option")
            .ignore_then(keyword_p())
            .map(Command::GetOption),
    ));

    let group_c = choice((
        kw("push").ignore_then(numeral_p()).map(Command::Push),
        kw("pop").ignore_then(numeral_p()).map(Command::Pop),
        kw("reset-assertions").to(Command::ResetAssertions),
        kw("reset").to(Command::Reset),
        kw("echo").ignore_then(string_lit_p()).map(Command::Echo),
        kw("exit").to(Command::Exit),
    ));

    lparen()
        .ignore_then(choice((group_a, group_b, group_c)))
        .then_ignore(rparen())
}

fn script_p<'a>() -> impl Parser<'a, &'a str, Script, Extra<'a>> + Clone {
    ws().ignore_then(command_p().repeated().collect::<Vec<_>>())
        .then_ignore(end())
        .map(Script)
}

/// Parse a full SMT-LIB script. Errors are rendered to strings.
pub fn parse_script(src: &str) -> Result<Script, Vec<String>> {
    script_p()
        .parse(src)
        .into_result()
        .map_err(|errs| errs.into_iter().map(|e| e.to_string()).collect())
}

/// Parse a single term (no surrounding script), for tests and term-level use.
pub fn parse_term(src: &str) -> Result<Term, Vec<String>> {
    ws().ignore_then(term_p())
        .then_ignore(end())
        .parse(src)
        .into_result()
        .map_err(|errs| errs.into_iter().map(|e| e.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bitvec_constant_term() {
        let t = parse_term("(_ bv13 8)").unwrap();
        assert_eq!(
            t,
            Term::Ident(QualIdentifier::Plain(Identifier {
                symbol: Symbol("bv13".into()),
                indices: vec![Index::Numeral(8)],
            }))
        );
    }

    #[test]
    fn parses_application_and_literals() {
        let t = parse_term("(bvadd #x0f #b1010)").unwrap();
        match t {
            Term::App(QualIdentifier::Plain(id), args) => {
                assert_eq!(id.symbol, Symbol("bvadd".into()));
                assert_eq!(args.len(), 2);
                assert_eq!(
                    args[0],
                    Term::Constant(SpecConstant::Hexadecimal("0f".into()))
                );
                assert_eq!(args[1], Term::Constant(SpecConstant::Binary("1010".into())));
            }
            other => panic!("expected app, got {other:?}"),
        }
    }

    #[test]
    fn parses_let_and_extract() {
        let t = parse_term("(let ((x #x0f)) ((_ extract 3 0) x))").unwrap();
        match t {
            Term::Let(binds, body) => {
                assert_eq!(binds.len(), 1);
                assert_eq!(binds[0].var, Symbol("x".into()));
                match *body {
                    Term::App(QualIdentifier::Plain(id), _) => {
                        assert_eq!(id.symbol, Symbol("extract".into()));
                        assert_eq!(id.indices, vec![Index::Numeral(3), Index::Numeral(0)]);
                    }
                    other => panic!("expected extract app, got {other:?}"),
                }
            }
            other => panic!("expected let, got {other:?}"),
        }
    }

    #[test]
    fn parses_forall_with_comment() {
        let t = parse_term("; a comment\n(forall ((x (_ BitVec 8))) (= x x))").unwrap();
        assert!(matches!(t, Term::Forall(_, _)));
    }

    #[test]
    fn parses_script() {
        let src = "(set-logic QF_BV)\n\
                   (declare-const x (_ BitVec 32))\n\
                   (assert (= (bvadd x #x00000001) x))\n\
                   (check-sat)\n\
                   (exit)";
        let script = parse_script(src).unwrap();
        assert_eq!(script.0.len(), 5);
        assert_eq!(script.0[0], Command::SetLogic(Symbol("QF_BV".into())));
        assert!(matches!(script.0[1], Command::DeclareConst(_, _)));
        assert!(matches!(script.0[2], Command::Assert(_)));
        assert_eq!(script.0[3], Command::CheckSat);
        assert_eq!(script.0[4], Command::Exit);
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse_term("(bvadd x y) extra").is_err());
    }

    #[test]
    fn enforces_numeral_and_keyword_lexis() {
        assert_eq!(
            parse_term("0").unwrap(),
            Term::Constant(SpecConstant::Numeral(0))
        );
        // No leading zeros (other than a bare `0`).
        assert!(parse_term("0123").is_err());
        assert!(parse_script("(push 00)").is_err());
        // Keywords may not start with a digit.
        assert!(parse_script("(set-info :123 x)").is_err());
        assert!(parse_script("(set-info :status sat)").is_ok());
    }
}
