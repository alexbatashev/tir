use std::collections::{HashMap, HashSet};

use chumsky::error::Rich;

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

// Checks that all ISA parents are defined and are also ISAs.
fn check_isas(files: &[ast::File], item_cache: &HashMap<&str, &ast::Item>) -> Vec<(String, Diag)> {
    fn check_isa_exists(
        parent: &str,
        file_name: String,
        child: &ast::Isa,
        item_cache: &HashMap<&str, &ast::Item>,
    ) -> Option<(String, Diag)> {
        let parent_item = item_cache.get(parent);
        if let Some(parent_item) = parent_item {
            if !matches!(parent_item, ast::Item::Isa(_)) {
                Some((
                    file_name,
                    Rich::custom(
                        child.span,
                        format!(
                            "Parent '{}' for ISA '{}' must also be an ISA",
                            parent, child.name
                        ),
                    ),
                ))
            } else {
                None
            }
        } else {
            Some((
                file_name,
                Rich::custom(
                    child.span,
                    format!("Unknown parent '{}' for ISA '{}'", parent, child.name),
                ),
            ))
        }
    }

    files
        .iter()
        .flat_map(|file| {
            file.items
                .iter()
                .filter_map(|item| match item {
                    ast::Item::Isa(isa) => {
                        if let Some(reqs) = &isa.requires {
                            match reqs {
                                ast::IsaRequirement::Single(parent) => Some(
                                    vec![check_isa_exists(
                                        parent.as_str(),
                                        file.file_name.clone(),
                                        isa,
                                        item_cache,
                                    )]
                                    .into_iter(),
                                ),
                                ast::IsaRequirement::All(isas) => Some(
                                    isas.iter()
                                        .map(|parent| {
                                            check_isa_exists(
                                                parent.as_str(),
                                                file.file_name.clone(),
                                                isa,
                                                item_cache,
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .into_iter(),
                                ),
                                ast::IsaRequirement::Any(isas) => Some(
                                    isas.iter()
                                        .map(|parent| {
                                            check_isa_exists(
                                                parent.as_str(),
                                                file.file_name.clone(),
                                                isa,
                                                item_cache,
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .into_iter(),
                                ),
                            }
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .flatten()
                .flatten()
        })
        .collect()
}

fn check_templates(
    files: &[ast::File],
    item_cache: &HashMap<&str, &ast::Item>,
) -> Vec<(String, Diag)> {
    let mut diags = vec![];

    diags.extend(files.iter().flat_map(|f| {
        f.templates()
            .flat_map(|t| check_template_parents(t, item_cache, &f.file_name).into_iter())
    }));

    diags
}

fn check_instructions(
    files: &[ast::File],
    item_cache: &HashMap<&str, &ast::Item>,
) -> Vec<(String, Diag)> {
    let mut diags = vec![];

    diags.extend(files.iter().flat_map(|f| {
        f.instructions()
            .flat_map(|i| check_instruction_consistent(i, item_cache, &f.file_name).into_iter())
    }));

    diags
}

// Checks that all parent templates exist and are also templates.
fn check_template_parents(
    template: &ast::Template,
    item_cache: &HashMap<&str, &ast::Item>,
    file_name: &str,
) -> Vec<(String, Diag)> {
    let mut diags = vec![];
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(template.name.clone());
    let mut ancestor_params: HashSet<String> = HashSet::new();

    let mut current = template;

    loop {
        let Some(parent_name) = &current.parent_template else {
            break;
        };

        match item_cache.get(parent_name.as_str()).copied() {
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
                if !visited.insert(parent_name.clone()) {
                    diags.push((
                        file_name.to_string(),
                        Rich::custom(
                            current.span,
                            format!("Cyclic template inheritance involving '{}'", parent_name),
                        ),
                    ));
                    break;
                }
                for param_name in parent_tmpl.params.keys() {
                    ancestor_params.insert(param_name.clone());
                }
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
        if ancestor_params.contains(param_name) && value.is_none() {
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
        let parent_template = item_cache.get(parent_name);
        if parent_template.is_none() {
            diags.push((
                file_name.to_string(),
                Rich::custom(
                    instruction.span,
                    format!(
                        "Unknown parent template '{}' for instruction '{}'",
                        parent_name, instruction.name
                    ),
                ),
            ));
        }
        if let Some(item) = parent_template
            && !matches!(item, ast::Item::Template(_))
        {
            diags.push((
                file_name.to_string(),
                Rich::custom(
                    instruction.span,
                    format!(
                        "Parent '{}' for instruction '{}' must be a template",
                        parent_name, instruction.name
                    ),
                ),
            ));
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

    // Collect the template chain (root-first), guarding against cycles already reported.
    let mut chain: Vec<&ast::Template> = Vec::new();
    {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut current_parent = instruction.parent_template.as_deref();
        while let Some(parent_name) = current_parent {
            if !visited.insert(parent_name) {
                break;
            }
            match item_cache.get(parent_name).copied() {
                Some(ast::Item::Template(t)) => {
                    chain.push(t);
                    current_parent = t.parent_template.as_deref();
                }
                _ => break,
            }
        }
        chain.reverse(); // Root-first.
    }

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
    let effective_encoding: &[ast::EncodingArm] = if !instruction.encoding.is_empty() {
        &instruction.encoding
    } else {
        chain
            .iter()
            .rev()
            .find(|t| !t.encoding.is_empty())
            .map(|t| t.encoding.as_slice())
            .unwrap_or(&[])
    };
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
    let effective_asm = instruction
        .asm
        .as_ref()
        .or_else(|| chain.iter().rev().find_map(|t| t.asm.as_ref()));
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

    for arm in encoding {
        match &arm.value {
            ast::Expr::Lit(_) => {}
            ast::Expr::Ident(id) => {
                if !known(&id.name) {
                    diags.push((
                        file_name.to_string(),
                        Rich::custom(
                            arm.span,
                            format!(
                                "Unknown '{}' in encoding of instruction '{}': \
                                 not a parameter or operand",
                                id.name, instruction.name
                            ),
                        ),
                    ));
                }
            }
            ast::Expr::Slice(slc) => {
                if let ast::Expr::Ident(base) = &*slc.base {
                    if !known(&base.name) {
                        diags.push((
                            file_name.to_string(),
                            Rich::custom(
                                arm.span,
                                format!(
                                    "Unknown '{}' in encoding of instruction '{}': \
                                     not a parameter or operand",
                                    base.name, instruction.name
                                ),
                            ),
                        ));
                    }
                } else {
                    diags.push((
                        file_name.to_string(),
                        Rich::custom(
                            arm.span,
                            format!(
                                "Encoding value in instruction '{}' must be a literal, \
                                 parameter, or operand reference",
                                instruction.name
                            ),
                        ),
                    ));
                }
            }
            ast::Expr::IndexAccess(idx) => {
                if let ast::Expr::Ident(base) = &*idx.base {
                    if !known(&base.name) {
                        diags.push((
                            file_name.to_string(),
                            Rich::custom(
                                arm.span,
                                format!(
                                    "Unknown '{}' in encoding of instruction '{}': \
                                     not a parameter or operand",
                                    base.name, instruction.name
                                ),
                            ),
                        ));
                    }
                } else {
                    diags.push((
                        file_name.to_string(),
                        Rich::custom(
                            arm.span,
                            format!(
                                "Encoding value in instruction '{}' must be a literal, \
                                 parameter, or operand reference",
                                instruction.name
                            ),
                        ),
                    ));
                }
            }
            _ => {
                diags.push((
                    file_name.to_string(),
                    Rich::custom(
                        arm.span,
                        format!(
                            "Encoding value in instruction '{}' must be a literal, \
                             parameter, or operand reference",
                            instruction.name
                        ),
                    ),
                ));
            }
        }
    }

    diags
}
