use std::collections::HashMap;

use chumsky::error::Rich;

use crate::{Span, Type, TypeEnv, TypeScheme, TypeVar, TypeVarGen, ast, utils};

type Diag = Rich<'static, String, Span>;

type TypeCache<'a> = HashMap<&'a ast::Expr, Type>;

/// Maps register class names to their resolved bit type (may contain TypeVars).
type SynonymTable = HashMap<String, Type>;

/// Perform type checking/inference for all files after parsing and basic semantic checks.
/// Returns a cache of expression types and diagnostics.
pub fn check(files: &[ast::File]) -> (TypeCache, Vec<(String, Diag)>) {
    let mut tvg = TypeVarGen::new();

    let isa_param_vars = build_isa_param_vars(files, &mut tvg);

    // Register class name -> bits<TypeVar>.
    let synonyms = build_synonym_table(files, &isa_param_vars);

    // TODO use &str
    let item_cache: HashMap<String, &ast::Item> = files
        .iter()
        .flat_map(|f| f.items.iter().map(|i| (i.name().to_string(), i)))
        .collect();

    // For each instruction, build the TypeEnv from its resolved operands, then check behavior.
    for file in files {
        for instr in file.instructions() {
            let _env = build_instr_env(instr, &item_cache, &synonyms);
            // TODO: walk instr.behavior, infer types for each Expr, unify against _env
        }
    }

    todo!()
}

/// Collect one TypeVar per unique ISA parameter name across all ISAs in all files.
fn build_isa_param_vars(files: &[ast::File], tvg: &mut TypeVarGen) -> HashMap<String, TypeVar> {
    let mut vars: HashMap<String, TypeVar> = HashMap::new();
    for file in files {
        for item in &file.items {
            if let ast::Item::Isa(isa) = item {
                for param_name in isa.parameters.keys() {
                    vars.entry(param_name.clone())
                        .or_insert_with(|| tvg.fresh());
                }
            }
        }
    }
    vars
}

/// Determine the bit type of a register class.
fn reg_class_type(rc: &ast::RegisterClass, isa_param_vars: &HashMap<String, TypeVar>) -> Type {
    if let Some((_ty, Some(default))) = rc.parameters.get("WIDTH") {
        if let ast::Expr::Field(field) = default {
            if let Some(&tv) = isa_param_vars.get(&field.member) {
                return Type::Con("bits".into(), vec![Type::Var(tv)]);
            }
        }
    }
    unreachable!("All register classes must have WIDTH parameter")
}

/// Build the synonym table: register class name → its bit type.
fn build_synonym_table(
    files: &[ast::File],
    isa_param_vars: &HashMap<String, TypeVar>,
) -> SynonymTable {
    let mut synonyms = SynonymTable::new();
    for file in files {
        for rc in file.register_classes() {
            synonyms.insert(rc.name.clone(), reg_class_type(rc, isa_param_vars));
        }
    }
    synonyms
}

/// Expand a `Struct("REG_CLASS")` through the synonym table to its underlying type.
fn normalize(ty: &Type, synonyms: &SynonymTable) -> Type {
    match ty {
        Type::Struct(name) => synonyms.get(name).cloned().unwrap_or_else(|| ty.clone()),
        other => other.clone(),
    }
}

/// Build a TypeEnv for an instruction by binding each operand name to its resolved type.
fn build_instr_env(
    instr: &ast::Instruction,
    item_cache: &HashMap<String, &ast::Item>,
    synonyms: &SynonymTable,
) -> TypeEnv {
    let mut env = TypeEnv::new();
    for (name, ty) in utils::resolve_operands_for_instruction(instr, item_cache) {
        env.bind(name, TypeScheme::mono(normalize(&ty, synonyms)));
    }
    env
}
