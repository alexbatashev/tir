use fcc::diagnostics::{Span, intern_file};
use fcc::lang_options::LangOptions;
use fcc::lexer::Token;
use logos::Logos;
use tir::graph::Dag;

fn diagnostics(source: &str, options: LangOptions) -> String {
    let file = intern_file("<sema-test>", source);
    let tokens = Token::lexer(source)
        .spanned()
        .map(|(token, span)| (token.unwrap(), Span::new(file, span.start)))
        .collect::<Vec<_>>();
    let ast = fcc::parser::parse(&tokens, options).expect("parse");
    let diagnostics = match fcc::sema::analyze(ast, options) {
        Ok(_) => panic!("expected semantic error"),
        Err(diagnostics) => diagnostics,
    };
    let mut output = Vec::new();
    for diagnostic in diagnostics {
        diagnostic.write(&mut output, false).unwrap();
    }
    String::from_utf8(output).unwrap()
}

fn typed_for(source: &str, march: &str) -> fcc::sema::TypedAst {
    let options: LangOptions = "c23".parse().unwrap();
    let file = intern_file("<typed-sema-test>", source);
    let tokens = Token::lexer(source)
        .spanned()
        .map(|(token, span)| (token.unwrap(), Span::new(file, span.start)))
        .collect::<Vec<_>>();
    let ast = fcc::parser::parse(&tokens, options).expect("parse");
    let target = fcc::sema::TargetProfile::for_march(march).unwrap();
    fcc::sema::analyze_with_target(ast, options, target).expect("sema")
}

fn accepts(source: &str, options: LangOptions) -> bool {
    let file = intern_file("<valid-sema-test>", source);
    let tokens = Token::lexer(source)
        .spanned()
        .map(|(token, span)| (token.unwrap(), Span::new(file, span.start)))
        .collect::<Vec<_>>();
    let ast = fcc::parser::parse(&tokens, options).expect("parse");
    fcc::sema::analyze(ast, options).is_ok()
}

#[test]
fn computes_named_struct_layout() {
    let typed = typed_for(
        "struct Pair { char tag; int value; }; int main(void) { return 0; }",
        "riscv64",
    );
    let pair = typed
        .records()
        .find(|record| record.name == "Pair")
        .unwrap();

    assert_eq!(pair.size, 8);
    assert_eq!(pair.align, 4);
    assert_eq!(pair.fields[0].offset, 0);
    assert_eq!(pair.fields[1].offset, 4);
}

#[test]
fn gives_anonymous_structs_distinct_compiler_names() {
    let typed = typed_for(
        "typedef struct { int value; } First; typedef struct { int value; } Second;",
        "riscv64",
    );
    let names = typed
        .records()
        .map(|record| record.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
    assert!(
        names
            .iter()
            .all(|name| name.starts_with("__fcc_anon_struct."))
    );
}

#[test]
fn resolves_struct_member_type() {
    let typed = typed_for(
        "struct Pair { int value; }; int read(void) { struct Pair pair; return pair.value; }",
        "riscv64",
    );
    let ast = typed.ast();
    let member = ast
        .postorder(ast.root().unwrap())
        .find(|node| ast.get_node(*node).kind == fcc::ast::AstKind::Member)
        .unwrap();
    let semantics = ast.get_annotation(member).unwrap();

    assert!(matches!(
        typed.types().kind(semantics.ty.unwrap()),
        fcc::sema::TypeKind::Integer(fcc::sema::IntegerKind::Int)
    ));
    assert_eq!(semantics.member_index, Some(0));
}

#[test]
fn resolves_pointer_member_against_the_tag_identity() {
    let typed = typed_for(
        "struct Other { char byte; }; struct Pair { int value; }; int read(struct Pair *pair) { return pair->value; }",
        "riscv64",
    );
    let ast = typed.ast();
    let member = ast
        .postorder(ast.root().unwrap())
        .find(|node| ast.get_node(*node).kind == fcc::ast::AstKind::Member)
        .unwrap();
    let semantics = ast.get_annotation(member).unwrap();

    assert!(matches!(
        typed.types().kind(semantics.ty.unwrap()),
        fcc::sema::TypeKind::Integer(fcc::sema::IntegerKind::Int)
    ));
}

#[test]
fn rejects_unknown_struct_member() {
    let output = diagnostics(
        "struct Pair { int value; }; int read(void) { struct Pair pair; return pair.missing; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0402]"), "{output}");
    assert!(output.contains("has no member named 'missing'"), "{output}");
}

#[test]
fn rejects_duplicate_struct_member() {
    let output = diagnostics(
        "struct Pair { int value; char value; };",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0201]"), "{output}");
    assert!(output.contains("redefinition of 'value'"), "{output}");
}

#[test]
fn rejects_object_with_incomplete_struct_type() {
    let output = diagnostics(
        "struct Pair; int read(void) { struct Pair pair; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0409]"), "{output}");
    assert!(output.contains("incomplete struct type"), "{output}");
}

#[test]
fn rejects_local_array_with_incomplete_type() {
    let output = diagnostics(
        "int main(void) { int values[]; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(
        output.contains("object 'values' has incomplete array type"),
        "{output}"
    );
}

#[test]
fn rejects_excess_array_initializers() {
    let output = diagnostics(
        "int main(void) { int values[1] = {11, 22}; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(
        output.contains("too many initializers for array"),
        "{output}"
    );
}

#[test]
fn rejects_excess_record_initializers() {
    let output = diagnostics(
        "struct Pair { int value; }; int main(void) { struct Pair pair = {11, 22}; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(
        output.contains("too many initializers for record"),
        "{output}"
    );
}

#[test]
fn rejects_redefinition_in_same_scope() {
    let output = diagnostics(
        "int main(void) { int value; int value; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0201]"), "{output}");
    assert!(output.contains("redefinition of 'value'"), "{output}");
    assert!(output.contains("previous declaration is here"), "{output}");
    assert!(output.contains("N3220) 6.7.1p4"), "{output}");
    assert!(output.contains("n3220.pdf"), "{output}");
}

#[test]
fn rejects_undeclared_identifier_before_codegen() {
    let output = diagnostics("int main(void) { return missing; }", "c17".parse().unwrap());

    assert!(output.contains("[E0200]"), "{output}");
    assert!(
        output.contains("undeclared identifier 'missing'"),
        "{output}"
    );
    assert!(output.contains("N2176) 6.5.1p2"), "{output}");
}

#[test]
fn rejects_arithmetic_with_void_operand() {
    let output = diagnostics(
        "void sink(void); int main(void) { return sink() + 1; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0402]"), "{output}");
    assert!(
        output.contains("operator '+' requires arithmetic operands"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.7p2"), "{output}");
}

#[test]
fn pointer_arithmetic_requires_complete_object_pointee() {
    let output = diagnostics(
        "int offset(void *pointer) { return pointer + 1 != pointer; }",
        "c23".parse().unwrap(),
    );

    assert!(
        output.contains("pointer arithmetic requires a pointer to a complete object type"),
        "{output}"
    );
}

#[test]
fn pointer_difference_requires_compatible_pointee_types() {
    let output = diagnostics(
        "long distance(int *left, char *right) { return left - right; }",
        "c23".parse().unwrap(),
    );

    assert!(
        output
            .contains("pointer subtraction requires pointers to compatible complete object types"),
        "{output}"
    );
}

#[test]
fn assignment_requires_modifiable_lvalue() {
    let output = diagnostics(
        "int main(void) { 1 = 2; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0403]"), "{output}");
    assert!(
        output.contains("left operand is not a modifiable lvalue"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.17.1p2"), "{output}");
}

#[test]
fn void_function_cannot_return_a_value() {
    let output = diagnostics("void stop(void) { return 1; }", "c23".parse().unwrap());

    assert!(output.contains("[E0505]"), "{output}");
    assert!(
        output.contains("void function must not return a value"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.8.7.5p1"), "{output}");
}

#[test]
fn break_requires_loop_or_switch() {
    let output = diagnostics(
        "int main(void) { break; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0503]"), "{output}");
    assert!(
        output.contains("break statement is not inside a loop or switch"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.8.7.4p1"), "{output}");
}

#[test]
fn if_condition_requires_scalar_type() {
    let output = diagnostics(
        "void sink(void); int main(void) { if (sink()) return 1; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0500]"), "{output}");
    assert!(
        output.contains("if condition must have scalar type"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.8.5.2p1"), "{output}");
}

#[test]
fn call_checks_argument_count() {
    let output = diagnostics(
        "int add(int left, int right); int main(void) { return add(1); }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0406]"), "{output}");
    assert!(
        output.contains("function 'add' expects 2 arguments but 1 was provided"),
        "{output}"
    );
    assert!(output.contains("previous declaration is here"), "{output}");
    assert!(output.contains("N3220) 6.5.3.3p2"), "{output}");
}

#[test]
fn goto_requires_label_in_same_function() {
    let output = diagnostics(
        "int main(void) { goto missing; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0204]"), "{output}");
    assert!(
        output.contains("use of undeclared label 'missing'"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.8.7.2p1"), "{output}");
}

#[test]
fn switch_rejects_duplicate_converted_case_values() {
    let output = diagnostics(
        "int main(int value) { switch (value) { case 1 + 1: return 1; case 2: return 2; } return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0502]"), "{output}");
    assert!(output.contains("duplicate case value 2"), "{output}");
    assert!(output.contains("previous case is here"), "{output}");
    assert!(output.contains("N3220) 6.8.5.3p3"), "{output}");
}

#[test]
fn for_initializer_declaration_has_loop_scope() {
    let output = diagnostics(
        "int main(void) { for (int index = 0; index < 1; index++) ; return index; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0200]"), "{output}");
    assert!(output.contains("undeclared identifier 'index'"), "{output}");
}

#[test]
fn rejects_invalid_type_specifier_combination() {
    let output = diagnostics(
        "int main(void) { unsigned float value; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0400]"), "{output}");
    assert!(
        output.contains("invalid type specifier combination 'unsigned float'"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.7.3.1p2"), "{output}");
}

#[test]
fn remainder_requires_integer_operands() {
    let output = diagnostics(
        "int remainder(float left, float right) { return left % right; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0402]"), "{output}");
    assert!(
        output.contains("operator '%' requires integer operands"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.6p2"), "{output}");
}

#[test]
fn assignment_rejects_nonzero_integer_to_pointer() {
    let output = diagnostics(
        "int main(void) { int *pointer; pointer = 1; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0404]"), "{output}");
    assert!(
        output.contains("cannot assign value of integer type to pointer"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.17.2p1"), "{output}");
}

#[test]
fn target_profile_controls_long_width() {
    let source = "long identity(long value) { return value; }";
    let ilp32 = typed_for(source, "riscv32");
    let lp64 = typed_for(source, "riscv64");
    let find_parameter_width = |typed: &fcc::sema::TypedAst| {
        let root = typed.ast().root().unwrap();
        typed
            .ast()
            .postorder(root)
            .find(|&node| typed.ast().get_node(node).kind == fcc::ast::AstKind::Param)
            .and_then(|node| typed.ast().get_annotation(node)?.ty)
            .map(|ty| typed.integer_width(ty).unwrap())
            .unwrap()
    };

    assert_eq!(find_parameter_width(&ilp32), 32);
    assert_eq!(find_parameter_width(&lp64), 64);
}

#[test]
fn rejects_invalid_integer_suffix() {
    let output = diagnostics("int main(void) { return 1ulul; }", "c23".parse().unwrap());

    assert!(output.contains("[E0401]"), "{output}");
    assert!(
        output.contains("invalid integer suffix in '1ulul'"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.4.4.1p1"), "{output}");
}

#[test]
fn rejects_conflicting_function_declarations() {
    let output = diagnostics(
        "int convert(int value); long convert(int value); int main(void) { return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0202]"), "{output}");
    assert!(
        output.contains("conflicting declarations for 'convert'"),
        "{output}"
    );
    assert!(output.contains("previous declaration is here"), "{output}");
    assert!(output.contains("N3220) 6.7.1p5"), "{output}");
}

#[test]
fn empty_parameter_list_is_a_prototype_only_in_c23() {
    let source = "int legacy(); int main(void) { return legacy(1); }";

    assert!(accepts(source, "c17".parse().unwrap()));
    let output = diagnostics(source, "c23".parse().unwrap());
    assert!(output.contains("[E0406]"), "{output}");
}

#[test]
fn call_requires_function_type() {
    let output = diagnostics(
        "int main(void) { int value; return value(); }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0405]"), "{output}");
    assert!(
        output.contains("called object 'value' is not a function"),
        "{output}"
    );
    assert!(output.contains("previous declaration is here"), "{output}");
    assert!(output.contains("N3220) 6.5.3.3p1"), "{output}");
}

#[test]
fn increment_requires_modifiable_lvalue() {
    let output = diagnostics("int main(void) { ++1; return 0; }", "c23".parse().unwrap());

    assert!(output.contains("[E0403]"), "{output}");
    assert!(
        output.contains("operand is not a modifiable lvalue"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.4.1p1"), "{output}");
}

#[test]
fn assignment_rejects_const_lvalue() {
    let output = diagnostics(
        "int main(void) { const int value = 1; value = 2; return value; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0403]"), "{output}");
    assert!(
        output.contains("left operand is not a modifiable lvalue"),
        "{output}"
    );
}

#[test]
fn restrict_requires_pointer_derived_object_type() {
    let output = diagnostics(
        "int main(void) { restrict int value; return value; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0408]"), "{output}");
    assert!(
        output.contains("restrict qualifier requires a pointer-derived object type"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.7.4.1p3"), "{output}");
}

#[test]
fn return_value_uses_assignment_constraints() {
    let output = diagnostics("int *invalid(void) { return 1; }", "c23".parse().unwrap());

    assert!(output.contains("[E0404]"), "{output}");
    assert!(
        output.contains("cannot return value of integer type as pointer"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.8.7.5p3"), "{output}");
}

#[test]
fn initializer_uses_assignment_constraints() {
    let output = diagnostics(
        "int main(void) { int *pointer = 1; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0404]"), "{output}");
    assert!(
        output.contains("cannot initialize pointer with integer value"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.7.11p12"), "{output}");
}

#[test]
fn sizeof_rejects_void_type() {
    let output = diagnostics(
        "int main(void) { return sizeof(void); }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0409]"), "{output}");
    assert!(
        output.contains("sizeof requires a complete object type"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.4.4p2"), "{output}");
}

#[test]
fn cast_requires_scalar_source_unless_target_is_void() {
    let output = diagnostics(
        "void sink(void); int main(void) { return (int)sink(); }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0404]"), "{output}");
    assert!(
        output.contains("cannot cast void expression to integer type"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.5p2"), "{output}");
}

#[test]
fn conditional_operator_requires_scalar_condition() {
    let output = diagnostics(
        "void sink(void); int main(void) { return sink() ? 1 : 2; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0402]"), "{output}");
    assert!(
        output.contains("conditional operator requires a scalar condition"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.5.16p2"), "{output}");
}

#[test]
fn call_checks_argument_types() {
    let output = diagnostics(
        "int read(int *pointer); int main(void) { return read(1); }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0404]"), "{output}");
    assert!(
        output.contains("argument 1 to 'read' has incompatible integer type"),
        "{output}"
    );
    assert!(output.contains("previous declaration is here"), "{output}");
    assert!(output.contains("N3220) 6.5.3.3p2"), "{output}");
}

#[test]
fn identifier_uses_are_bound_to_their_declarations() {
    let typed = typed_for(
        "int choose(int value) { { int value = 2; value = 3; } return value; }",
        "riscv64",
    );
    let root = typed.ast().root().unwrap();
    let nodes = typed.ast().postorder(root).collect::<Vec<_>>();
    let parameter = nodes
        .iter()
        .copied()
        .find(|&node| typed.ast().get_node(node).kind == fcc::ast::AstKind::Param)
        .unwrap();
    let local = nodes
        .iter()
        .copied()
        .find(|&node| typed.ast().get_node(node).kind == fcc::ast::AstKind::Decl)
        .unwrap();
    let uses = nodes
        .iter()
        .copied()
        .filter(|&node| {
            matches!(
                typed.ast().get_node(node).kind,
                fcc::ast::AstKind::Assign | fcc::ast::AstKind::Var
            )
        })
        .collect::<Vec<_>>();

    let parameter_entity = typed.ast().get_annotation(parameter).unwrap().entity;
    let local_entity = typed.ast().get_annotation(local).unwrap().entity;
    assert_ne!(parameter_entity, local_entity);
    assert_eq!(
        typed.ast().get_annotation(uses[0]).unwrap().entity,
        local_entity
    );
    assert_eq!(
        typed.ast().get_annotation(uses[1]).unwrap().entity,
        parameter_entity
    );
}

#[test]
fn usual_arithmetic_conversions_are_recorded() {
    let typed = typed_for(
        "long add(long left, int right) { return left + right; }",
        "riscv64",
    );
    let root = typed.ast().root().unwrap();
    let add = typed
        .ast()
        .postorder(root)
        .find(|&node| typed.ast().get_node(node).kind == fcc::ast::AstKind::Add)
        .unwrap();
    let result = typed.ast().get_annotation(add).unwrap().ty.unwrap();
    let operands = typed.ast().children(add).collect::<Vec<_>>();

    assert!(
        typed
            .ast()
            .get_annotation(operands[0])
            .unwrap()
            .conversions
            .is_empty()
    );
    assert_eq!(
        typed.ast().get_annotation(operands[1]).unwrap().conversions,
        vec![result]
    );
}

#[test]
fn usual_arithmetic_conversions_follow_the_target_data_model() {
    let source = "long mix(long signed_value, unsigned int unsigned_value) { return signed_value + unsigned_value; }";
    let result_kind = |typed: &fcc::sema::TypedAst| {
        let root = typed.ast().root().unwrap();
        let add = typed
            .ast()
            .postorder(root)
            .find(|&node| typed.ast().get_node(node).kind == fcc::ast::AstKind::Add)
            .unwrap();
        let ty = typed.ast().get_annotation(add).unwrap().ty.unwrap();
        typed.types().kind(ty).clone()
    };

    assert_eq!(
        result_kind(&typed_for(source, "riscv32")),
        fcc::sema::TypeKind::Integer(fcc::sema::IntegerKind::UnsignedLong)
    );
    assert_eq!(
        result_kind(&typed_for(source, "riscv64")),
        fcc::sema::TypeKind::Integer(fcc::sema::IntegerKind::Long)
    );
}

#[test]
fn assignment_like_contexts_record_the_destination_conversion() {
    let typed = typed_for(
        "long widen(int value) { long copy = value; return copy; }",
        "riscv64",
    );
    let root = typed.ast().root().unwrap();
    let declaration = typed
        .ast()
        .postorder(root)
        .find(|&node| typed.ast().get_node(node).kind == fcc::ast::AstKind::Decl)
        .unwrap();
    let destination = typed.ast().get_annotation(declaration).unwrap().ty.unwrap();
    let initializer = typed.ast().children(declaration).next().unwrap();

    assert_eq!(
        typed.ast().get_annotation(initializer).unwrap().conversions,
        vec![destination]
    );
}

#[test]
fn object_declaration_requires_an_object_type() {
    let output = diagnostics(
        "int main(void) { void value; return 0; }",
        "c23".parse().unwrap(),
    );

    assert!(output.contains("[E0409]"), "{output}");
    assert!(
        output.contains("object 'value' cannot have void type"),
        "{output}"
    );
    assert!(output.contains("N3220) 6.2.5p24"), "{output}");
}
