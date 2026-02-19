use std::collections::{HashMap, HashSet};

use chumsky::error::Rich;

use crate::utils::{
    resolve_effective_asm_for_instruction, resolve_effective_encoding_for_instruction,
    resolve_template_chain,
};
use crate::{Span, Type, ast};

type Diag = Rich<'static, String, Span>;

// TODO path strings must be interned
pub fn analyze(files: &[ast::File]) -> Vec<(String, Diag)> {
    let mut diags = vec![];

    let cache = build_item_cache(files);

    // TODO check item names are unique
    diags.extend(check_isas(files, &cache));
    diags.extend(check_templates(files, &cache));
    diags.extend(check_instructions(files, &cache));

    diags
}

fn build_item_cache<'a>(files: &'a [ast::File]) -> HashMap<&'a str, &'a ast::Item> {
    files
        .iter()
        .flat_map(|f| f.items.iter().map(|i| (i.name(), i)))
        .collect::<HashMap<_, _>>()
}

fn isa_parents(requirement: &ast::IsaRequirement) -> Vec<&str> {
    match requirement {
        ast::IsaRequirement::Single(parent) => vec![parent.as_str()],
        ast::IsaRequirement::All(parents) | ast::IsaRequirement::Any(parents) => {
            parents.iter().map(String::as_str).collect()
        }
    }
}

fn encoding_value_name(expr: &ast::Expr) -> Option<&str> {
    match expr {
        ast::Expr::Ident(id) => Some(id.name.as_str()),
        ast::Expr::Slice(slc) => match &*slc.base {
            ast::Expr::Ident(id) => Some(id.name.as_str()),
            _ => None,
        },
        ast::Expr::IndexAccess(idx) => match &*idx.base {
            ast::Expr::Ident(id) => Some(id.name.as_str()),
            _ => None,
        },
        _ => None,
    }
}

// Checks that all ISA parents are defined and are also ISAs.
fn check_isas(files: &[ast::File], item_cache: &HashMap<&str, &ast::Item>) -> Vec<(String, Diag)> {
    files
        .iter()
        .flat_map(|file| {
            file.isas().flat_map(|isa| {
                isa.requires
                    .as_ref()
                    .map(isa_parents)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|parent| match item_cache.get(parent) {
                        None => Some((
                            file.file_name.clone(),
                            Rich::custom(
                                isa.span,
                                format!("Unknown parent '{}' for ISA '{}'", parent, isa.name),
                            ),
                        )),
                        Some(item) if !matches!(item, ast::Item::Isa(_)) => Some((
                            file.file_name.clone(),
                            Rich::custom(
                                isa.span,
                                format!(
                                    "Parent '{}' for ISA '{}' must also be an ISA",
                                    parent, isa.name
                                ),
                            ),
                        )),
                        _ => None,
                    })
            })
        })
        .collect()
}

fn check_templates(
    files: &[ast::File],
    item_cache: &HashMap<&str, &ast::Item>,
) -> Vec<(String, Diag)> {
    files
        .iter()
        .flat_map(|f| {
            f.templates()
                .flat_map(|t| check_template_parents(t, item_cache, &f.file_name).into_iter())
        })
        .collect()
}

fn check_instructions(
    files: &[ast::File],
    item_cache: &HashMap<&str, &ast::Item>,
) -> Vec<(String, Diag)> {
    files
        .iter()
        .flat_map(|f| {
            f.instructions()
                .flat_map(|i| check_instruction_consistent(i, item_cache, &f.file_name).into_iter())
        })
        .collect()
}

// Checks that all parent templates exist and are also templates.
fn check_template_parents(
    template: &ast::Template,
    item_cache: &HashMap<&str, &ast::Item>,
    file_name: &str,
) -> Vec<(String, Diag)> {
    let mut diags = vec![];
    let mut visited: HashSet<&str> = HashSet::new();
    visited.insert(template.name.as_str());
    let mut ancestor_params: HashSet<&str> = HashSet::new();

    let mut current = template;

    loop {
        let Some(parent_name) = current.parent_template.as_deref() else {
            break;
        };

        match item_cache.get(parent_name).copied() {
            None => {
                diags.push((
                    file_name.to_string(),
                    Rich::custom(
                        current.span,
                        format!(
                            "Unknown parent template '{}' for template '{}'",
                            parent_name, &current.name
                        ),
                    ),
                ));
                break;
            }
            Some(ast::Item::Template(parent_tmpl)) => {
                if !visited.insert(parent_name) {
                    diags.push((
                        file_name.to_string(),
                        Rich::custom(
                            current.span,
                            format!("Cyclic template inheritance involving '{}'", parent_name),
                        ),
                    ));
                    break;
                }
                ancestor_params.extend(parent_tmpl.params.keys().map(String::as_str));
                current = parent_tmpl;
            }
            Some(_) => {
                diags.push((
                    file_name.to_string(),
                    Rich::custom(
                        current.span,
                        format!(
                            "Parent '{}' of template '{}' must also be a template",
                            parent_name, current.name
                        ),
                    ),
                ));
                break;
            }
        }
    }

    for (param_name, (_ty, value)) in &template.params {
        if ancestor_params.contains(param_name.as_str()) && value.is_none() {
            diags.push((
                file_name.to_string(),
                Rich::custom(
                    template.span,
                    format!(
                        "Parameter '{}' in template '{}' is already defined by an ancestor; \
                         provide a value to override it",
                        param_name, template.name
                    ),
                ),
            ));
        }
    }

    diags
}

fn check_instruction_consistent(
    instruction: &ast::Instruction,
    item_cache: &HashMap<&str, &ast::Item>,
    file_name: &str,
) -> Vec<(String, Diag)> {
    let mut diags = vec![];

    // Check parent template exists and is a template.
    if let Some(parent_name) = instruction.parent_template.as_ref().map(|n| n.as_str()) {
        match item_cache.get(parent_name).copied() {
            None => diags.push((
                file_name.to_string(),
                Rich::custom(
                    instruction.span,
                    format!(
                        "Unknown parent template '{}' for instruction '{}'",
                        parent_name, instruction.name
                    ),
                ),
            )),
            Some(item) if !matches!(item, ast::Item::Template(_)) => diags.push((
                file_name.to_string(),
                Rich::custom(
                    instruction.span,
                    format!(
                        "Parent '{}' for instruction '{}' must be a template",
                        parent_name, instruction.name
                    ),
                ),
            )),
            _ => {}
        }
    }

    // Check ISAs exist and are ISAs.
    for isa_name in &instruction.for_isas {
        match item_cache.get(isa_name.as_str()).copied() {
            None => {
                diags.push((
                    file_name.to_string(),
                    Rich::custom(
                        instruction.span,
                        format!(
                            "Unknown ISA '{}' in instruction '{}'",
                            isa_name, instruction.name
                        ),
                    ),
                ));
            }
            Some(item) if !matches!(item, ast::Item::Isa(_)) => {
                diags.push((
                    file_name.to_string(),
                    Rich::custom(
                        instruction.span,
                        format!(
                            "'{}' referenced in instruction '{}' is not an ISA",
                            isa_name, instruction.name
                        ),
                    ),
                ));
            }
            _ => {}
        }
    }

    let chain = resolve_template_chain(instruction, item_cache);

    // Build params_cache: root-first insertion means later (closer) definitions win.
    let mut params_cache: HashMap<&str, (Type, Option<ast::Expr>)> = HashMap::new();
    for tmpl in &chain {
        for (name, (ty, value)) in &tmpl.params {
            params_cache.insert(name.as_str(), (ty.clone(), value.clone()));
        }
    }
    for (name, (ty, value)) in &instruction.params {
        params_cache.insert(name.as_str(), (ty.clone(), value.clone()));
    }

    // Build operands_cache from chain + instruction.
    let mut operands_cache: HashMap<&str, Type> = HashMap::new();
    for tmpl in &chain {
        for (name, ty) in &tmpl.operands {
            operands_cache.insert(name.as_str(), ty.clone());
        }
    }
    for (name, ty) in &instruction.operands {
        operands_cache.insert(name.as_str(), ty.clone());
    }

    for (name, (_ty, value)) in &params_cache {
        if value.is_none() {
            diags.push((
                file_name.to_string(),
                Rich::custom(
                    instruction.span,
                    format!(
                        "Parameter '{}' in instruction '{}' has no bound value",
                        name, instruction.name
                    ),
                ),
            ));
        }
    }

    // Encoding must exist somewhere in the chain or instruction.
    let effective_encoding = resolve_effective_encoding_for_instruction(instruction, item_cache);
    if effective_encoding.is_empty() {
        diags.push((
            file_name.to_string(),
            Rich::custom(
                instruction.span,
                format!("Instruction '{}' has no encoding defined", instruction.name),
            ),
        ));
    } else {
        diags.extend(check_encoding(
            instruction,
            effective_encoding,
            &params_cache,
            &operands_cache,
            file_name,
        ));
    }

    // Asm must exist somewhere in the chain or instruction.
    let effective_asm = resolve_effective_asm_for_instruction(instruction, item_cache);
    if effective_asm.is_none() {
        diags.push((
            file_name.to_string(),
            Rich::custom(
                instruction.span,
                format!(
                    "Instruction '{}' has no asm block defined",
                    instruction.name
                ),
            ),
        ));
    } else {
        diags.extend(check_asm(
            instruction,
            effective_asm.unwrap(),
            &params_cache,
            file_name,
        ));
    }

    diags.extend(check_behavior(
        instruction,
        &instruction.behavior,
        &params_cache,
        file_name,
    ));

    diags
}

fn check_asm(
    instruction: &ast::Instruction,
    asm_: &ast::Expr,
    _params_cache: &HashMap<&str, (Type, Option<ast::Expr>)>,
    file_name: &str,
) -> Vec<(String, Diag)> {
    // Asm may be wrapped in a block (`asm { "..." }`); unwrap a single-expression block.
    let inner = match asm_ {
        ast::Expr::Block(b) if b.stmts.len() == 1 => &b.stmts[0],
        other => other,
    };
    match inner {
        ast::Expr::Lit(ast::Lit::Str(_)) => vec![],
        _ => vec![(
            file_name.to_string(),
            Rich::custom(
                instruction.span,
                format!(
                    "Asm block must be a single literal string for instruction '{}'",
                    instruction.name
                ),
            ),
        )],
    }
}

fn check_behavior(
    _instruction: &ast::Instruction,
    _behavior: &ast::Expr,
    _params_cache: &HashMap<&str, (Type, Option<ast::Expr>)>,
    _file_name: &str,
) -> Vec<(String, Diag)> {
    // Always fine for now
    vec![]
}

fn check_encoding(
    instruction: &ast::Instruction,
    encoding: &[ast::EncodingArm],
    params_cache: &HashMap<&str, (Type, Option<ast::Expr>)>,
    operands_cache: &HashMap<&str, Type>,
    file_name: &str,
) -> Vec<(String, Diag)> {
    let mut diags = vec![];

    let known = |name: &str| params_cache.contains_key(name) || operands_cache.contains_key(name);
    let invalid_value = |span: Span| {
        (
            file_name.to_string(),
            Rich::custom(
                span,
                format!(
                    "Encoding value in instruction '{}' must be a literal, \
                     parameter, or operand reference",
                    instruction.name
                ),
            ),
        )
    };
    let unknown_value = |name: &str, span: Span| {
        (
            file_name.to_string(),
            Rich::custom(
                span,
                format!(
                    "Unknown '{}' in encoding of instruction '{}': \
                     not a parameter or operand",
                    name, instruction.name
                ),
            ),
        )
    };

    for arm in encoding {
        if let ast::Expr::Lit(_) = arm.value {
            continue;
        }

        match encoding_value_name(&arm.value) {
            Some(name) if !known(name) => diags.push(unknown_value(name, arm.span)),
            Some(_) => {}
            None => {
                diags.push(invalid_value(arm.span));
            }
        }
    }

    diags
}
