use std::collections::{HashMap, HashSet};
use std::io::Write;

use quote::{format_ident, quote};
use tir::graph::Dag;

use crate::Type;
use crate::ast;
use crate::error::TMDLError;
use crate::utils::{
    get_encoding_arms, parse_literal_value, resolve_effective_asm_for_instruction,
    resolve_operands_for_instruction, resolve_params_for_instruction,
};

pub fn generate_rust<'a>(
    dialect: &str,
    files: &'a [ast::File],
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    mut output: Box<dyn Write>,
) -> Result<(), TMDLError> {
    let features = emit_features(files)?;
    let register_traits = emit_register_trait_helpers(files)?;
    let registers = emit_register_parsers_and_printers(files)?;
    let instructions = emit_instructions(dialect, files, item_cache)?;

    let final_rust = quote! {
        #features
        #register_traits

        #registers

        #instructions
    };

    let syntax_tree = syn::parse2(final_rust).unwrap();
    let formatted = prettyplease::unparse(&syntax_tree);

    output.write(formatted.as_bytes())?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Top-level emitters
// ---------------------------------------------------------------------------

fn emit_features(files: &[ast::File]) -> Result<proc_macro2::TokenStream, TMDLError> {
    let mut enum_variants = vec![];
    let mut name_arms = vec![];

    for isa in files.iter().flat_map(|f| f.isas()) {
        let ident = format_ident!("{}", &isa.name);
        let name = isa.name.clone();
        enum_variants.push(quote! { #ident });
        name_arms.push(quote! { Self::#ident => #name });
    }

    Ok(quote! {
        pub enum Feature {
            #(#enum_variants,)*
            Custom,
        }

        impl Feature {
            pub fn name(&self) -> &'static str {
                match self {
                    #(#name_arms,)*
                    Feature::Custom => "custom",
                }
            }
        }
    })
}

fn emit_instructions<'a>(
    dialect: &str,
    files: &'a [ast::File],
    item_cache: &HashMap<&'a str, &'a ast::Item>,
) -> Result<proc_macro2::TokenStream, TMDLError> {
    let mut instruction_defs = vec![];
    let mut instruction_parsers_impls: Vec<proc_macro2::TokenStream> = vec![];
    let mut instruction_parser_map_inits: Vec<proc_macro2::TokenStream> = vec![];
    let mut isel_rule_emitters: Vec<proc_macro2::TokenStream> = vec![];
    let mut isel_rule_inits: Vec<proc_macro2::TokenStream> = vec![];
    let mut machine_instruction_impls: Vec<proc_macro2::TokenStream> = vec![];
    let mut instruction_custom_format_impls: Vec<proc_macro2::TokenStream> = vec![];
    let mut as_sem_expr_impls: Vec<proc_macro2::TokenStream> = vec![];

    for inst in files.iter().flat_map(|f| f.instructions()) {
        let name_ident = format_ident!("{}Op", &inst.name);
        let builder_ident = format_ident!("{}OpBuilder", &inst.name);
        let resolved_params = resolve_params_for_instruction(inst, item_cache);
        let mnemonic = resolved_params
            .get("MNEMONIC")
            .and_then(|(_, value)| value.as_ref())
            .and_then(resolve_string);
        let opname = resolved_params
            .get("OPNAME")
            .and_then(|(_, value)| value.as_ref())
            .and_then(resolve_string);

        let op_name = if let Some(opname) = opname.as_deref() {
            opname
        } else if let Some(mnemonic) = mnemonic.as_deref() {
            mnemonic
        } else {
            return Err(TMDLError::Codegen(format!(
                "Instruction '{}' must define OPNAME or MNEMONIC",
                inst.name
            )));
        };

        let mnemonic_name = mnemonic.as_deref().unwrap_or(op_name);
        let op_name_lit = proc_macro2::Literal::string(op_name);
        let ops = resolve_operands_for_instruction(inst, item_cache);
        let ops_map = ops.clone().into_iter().collect::<HashMap<_, _>>();
        let defined_register_operands = infer_defined_register_operands(&inst.behavior, &ops);

        // Build attributes schema from operands
        let attrs_schema = {
            let mut items = vec![];
            for (name, ty) in &ops {
                let field_ident = format_ident!("{}", name);
                let ty_ts = match ty {
                    Type::Struct(_) => quote! { Register },
                    Type::Integer | Type::Bits(_) => quote! { Integer },
                    Type::String => quote! { String },
                    _ => unreachable!("HM type vars should not appear as operand types"),
                };
                items.push(quote! { #field_ident: #ty_ts });
            }
            quote! { #(#items,)* }
        };

        // Build roles from behavior assignments so we don't depend on naming conventions.
        let roles_schema = {
            let mut items = vec![];
            for (name, ty) in &ops {
                if let Type::Struct(_) = ty {
                    let field_ident = format_ident!("{}", name);
                    let role = if defined_register_operands.contains(name) {
                        quote! { Def }
                    } else {
                        quote! { Use }
                    };
                    items.push(quote! { #field_ident: #role });
                }
            }
            quote! { #(#items,)* }
        };

        instruction_defs.push(quote! {
            operation! {
                #name_ident {
                    name: #op_name_lit,
                    dialect: #dialect,
                    attributes: A { #attrs_schema },
                    roles: R { #roles_schema },
                    interfaces: [tir_be_common::MachineInstruction],
                    format: custom,
                }
            }
        });

        let op_display_name = format!("{}.{}", dialect, op_name);
        let op_display_name_lit = proc_macro2::Literal::string(&op_display_name);
        let mut register_attr_print_arms = Vec::new();
        for (op_name, op_ty) in &ops {
            if let Type::Struct(class_name) = op_ty {
                let attr_name_lit = proc_macro2::Literal::string(op_name);
                let class_lit = proc_macro2::Literal::string(class_name);
                let print_fn_ident = format_ident!("print_{}", class_name.to_lowercase());
                register_attr_print_arms.push(quote! {
                    #attr_name_lit => {
                        if let tir::attributes::AttributeValue::Register(tir::attributes::RegisterAttr::Physical { class, index }) = &attr.value {
                            if class == #class_lit {
                                if let Some(name) = #print_fn_ident(*index, false) {
                                    fmt.write(name)?;
                                } else {
                                    attr.value.print(fmt, &context)?;
                                }
                            } else {
                                attr.value.print(fmt, &context)?;
                            }
                        } else {
                            attr.value.print(fmt, &context)?;
                        }
                    }
                });
            }
        }
        instruction_custom_format_impls.push(quote! {
            impl #name_ident {
                fn custom_print<'a, 'b: 'a>(
                    &'a self,
                    fmt: &'a mut tir::IRFormatter<'b>,
                ) -> Result<(), std::fmt::Error> {
                    use tir::Operation;

                    fmt.write(#op_display_name_lit)?;
                    if !self.attributes().is_empty() {
                        fmt.write(" ")?;
                        fmt.write("{")?;
                        let mut first = true;
                        let context = self.0.context.upgrade();
                        for attr in self.attributes() {
                            if !first {
                                fmt.write(", ")?;
                            }
                            first = false;
                            fmt.write(&attr.name)?;
                            fmt.write(" = ")?;
                            match attr.name.as_str() {
                                #(#register_attr_print_arms,)*
                                _ => attr.value.print(fmt, &context)?,
                            }
                        }
                        fmt.write("}")?;
                    }
                    fmt.write("\n")?;
                    Ok(())
                }

                fn custom_parse<'src>(
                    parser: &mut tir::parse::text::Parser<'src>,
                    _context: &tir::Context,
                ) -> Result<Box<dyn tir::Operation>, (tir::parse::Span, tir::Error)> {
                    Err((tir::parse::Span(parser.pos()), tir::Error::ExpectedOpName))
                }
            }
        });

        let numeric_params: HashMap<String, i64> = resolve_params_for_instruction(inst, item_cache)
            .into_iter()
            .filter_map(|(name, (_ty, value))| match value {
                Some(ast::Expr::Lit(ast::Lit::Int(li))) => {
                    Some((name, parse_literal_value(&li) as i64))
                }
                _ => None,
            })
            .collect();

        if let Some(semantics) =
            analyze_instruction_semantics(inst, &ops, &defined_register_operands, &numeric_params)
        {
            let emit_fn_ident = format_ident!("emit_isel_{}", inst.name.to_lowercase());
            let pattern_fn_ident = format_ident!("isel_pattern_{}", inst.name.to_lowercase());
            let rule_name_lit = proc_macro2::Literal::string(&inst.name.to_lowercase());
            let mut emit_attr_steps = Vec::new();
            for (op_name, op_ty) in &ops {
                let op_name_lit = proc_macro2::Literal::string(op_name);
                match op_ty {
                    Type::Struct(class_name) => {
                        let class_lit = proc_macro2::Literal::string(class_name);
                        if let Some(def_pos) = defined_register_operands
                            .iter()
                            .position(|name| name == op_name)
                        {
                            let def_pos_lit = proc_macro2::Literal::usize_unsuffixed(def_pos);
                            emit_attr_steps.push(quote! {
                                let dst = op
                                    .op()
                                    .results
                                    .get(#def_pos_lit)
                                    .ok_or(tir::PassError::RewriteFailed(op.op().id))?
                                    .number();
                                builder = builder.attr(
                                    #op_name_lit,
                                    tir::attributes::AttributeValue::Register(
                                        tir::attributes::RegisterAttr::Virtual {
                                            id: dst,
                                            class: Some(#class_lit.to_string()),
                                        },
                                    ),
                                );
                            });
                        } else if let Some(sym) = semantics.variable_symbols.get(op_name) {
                            let sym_lit = proc_macro2::Literal::u32_unsuffixed(*sym);
                            emit_attr_steps.push(quote! {
                                let src = m.value_binding(#sym_lit).ok_or(tir::PassError::RewriteFailed(op.op().id))?;
                                builder = builder.attr(
                                    #op_name_lit,
                                    tir::attributes::AttributeValue::Register(
                                        tir::attributes::RegisterAttr::Virtual {
                                            id: src.number(),
                                            class: Some(#class_lit.to_string()),
                                        },
                                    ),
                                );
                            });
                        } else if let Some(Some(reg_idx)) =
                            semantics.fixed_register_by_class.get(class_name)
                        {
                            let idx_lit = proc_macro2::Literal::u16_unsuffixed(*reg_idx);
                            emit_attr_steps.push(quote! {
                                builder = builder.attr(
                                    #op_name_lit,
                                    tir::attributes::AttributeValue::Register(
                                        tir::attributes::RegisterAttr::Physical {
                                            class: #class_lit.to_string(),
                                            index: #idx_lit,
                                        },
                                    ),
                                );
                            });
                        }
                    }
                    Type::Integer | Type::Bits(_) => {
                        if let Some(sym) = semantics.variable_symbols.get(op_name) {
                            let sym_lit = proc_macro2::Literal::u32_unsuffixed(*sym);
                            emit_attr_steps.push(quote! {
                                let v = m.int_binding(#sym_lit).ok_or(tir::PassError::RewriteFailed(op.op().id))?;
                                builder = builder.attr(
                                    #op_name_lit,
                                    tir::attributes::AttributeValue::Int(v),
                                );
                            });
                        }
                    }
                    Type::String => {}
                    _ => {}
                }
            }

            let (pattern_stmts, _root_var) = emit_dag_as_code(&semantics.pattern, semantics.root);
            let base_cost_lit = proc_macro2::Literal::u32_unsuffixed(semantics.base_cost);
            isel_rule_emitters.push(quote! {
                fn #pattern_fn_ident() -> tir::sem_expr2::ExprPostGraph {
                    use tir::graph::MutDag;
                    let mut g = tir::sem_expr2::ExprPostGraph::new();
                    #(#pattern_stmts)*
                    g
                }

                fn #emit_fn_ident(
                    context: &tir::Context,
                    op: &tir::OperationRef,
                    m: &tir_be_common::isel::RuleMatch,
                ) -> Result<tir_be_common::isel::EmitPlan, tir::PassError> {
                    let mut builder = #builder_ident::new(context);
                    #(#emit_attr_steps)*
                    Ok(tir_be_common::isel::EmitPlan::single(Box::new(builder.build())))
                }
            });

            isel_rule_inits.push(quote! {
                rules.push(tir_be_common::isel::Rule::new(
                    #rule_name_lit,
                    #pattern_fn_ident(),
                    #base_cost_lit,
                    #emit_fn_ident,
                ));
            });
        }

        if let Some(impl_ts) = emit_as_sem_expr_impl(
            inst,
            &ops,
            &defined_register_operands,
            &name_ident,
            &numeric_params,
        ) {
            as_sem_expr_impls.push(impl_ts);
        }

        let encoding_arms = get_encoding_arms(inst, item_cache);
        let encoding_bits = encoding_arms
            .iter()
            .map(|arm| arm.end.unwrap_or(arm.start))
            .max()
            .map(|max_end| max_end + 1)
            .unwrap_or(32);
        let width_bytes_lit =
            proc_macro2::Literal::u8_unsuffixed(((encoding_bits as u32 + 7) / 8) as u8);
        let mnemonic_lit = proc_macro2::Literal::string(mnemonic_name);

        let execute_body = if let Some(rhs) =
            resolve_behavior_rhs(inst, &ops, &defined_register_operands)
        {
            let dst_write = if let Some(dst_name) = defined_register_operands.last() {
                let dst_lit = proc_macro2::Literal::string(dst_name);
                quote! {
                    let (dst_class, dst_idx) = tir_be_common::register_attr(self.attributes(), #dst_lit).ok_or(
                        tir_be_common::SimTrap::MissingAttribute {
                            op: #mnemonic_lit,
                            attribute: #dst_lit,
                        },
                    )?;
                    if !register_has_trait_hardwired_zero(&dst_class, dst_idx) {
                        machine.write_register(&dst_class, dst_idx, value)?;
                    }
                }
            } else {
                quote! {}
            };

            let mut dag = tir::sem_expr2::ExprPostGraph::new();
            if let Some(lowering) = rhs.lower_to_sema(&mut dag, &numeric_params) {
                let max_sym_id = [
                    lowering.variable_symbols.values().copied().max(),
                    lowering.register_symbols.values().copied().max(),
                ]
                .into_iter()
                .flatten()
                .max()
                .unwrap_or(0) as usize;
                let max_sym_id_lit = proc_macro2::Literal::usize_unsuffixed(max_sym_id);

                let mut sym_init_steps: Vec<proc_macro2::TokenStream> = Vec::new();
                for (name, &sym_id) in &lowering.variable_symbols {
                    let sym_lit = proc_macro2::Literal::usize_unsuffixed(sym_id as usize);
                    let name_lit = proc_macro2::Literal::string(name);
                    if let Some((_, ty)) = ops.iter().find(|(n, _)| n == name) {
                        match ty {
                            Type::Struct(_) => sym_init_steps.push(quote! {
                                {
                                    let (class, index) = tir_be_common::register_attr(self.attributes(), #name_lit)
                                        .ok_or(tir_be_common::SimTrap::MissingAttribute {
                                            op: #mnemonic_lit,
                                            attribute: #name_lit,
                                        })?;
                                    __syms[#sym_lit] = Some(tir::sem_expr2::Value::Int(machine.read_register(&class, index)?));
                                }
                            }),
                            Type::Integer => sym_init_steps.push(quote! {
                                {
                                    let value = tir_be_common::int_attr(self.attributes(), #name_lit)
                                        .ok_or(tir_be_common::SimTrap::MissingAttribute {
                                            op: #mnemonic_lit,
                                            attribute: #name_lit,
                                        })?;
                                    __syms[#sym_lit] = Some(tir::sem_expr2::Value::Int(tir::utils::APInt::new_signed(64, value)));
                                }
                            }),
                            Type::Bits(width) => {
                                let width_lit =
                                    proc_macro2::Literal::u32_unsuffixed(*width as u32);
                                sym_init_steps.push(quote! {
                                    {
                                        let value = tir_be_common::int_attr(self.attributes(), #name_lit)
                                            .ok_or(tir_be_common::SimTrap::MissingAttribute {
                                                op: #mnemonic_lit,
                                                attribute: #name_lit,
                                            })?;
                                        __syms[#sym_lit] = Some(tir::sem_expr2::Value::Int(tir::utils::APInt::new_signed(#width_lit, value)));
                                    }
                                });
                            }
                            _ => {}
                        }
                    }
                }
                for ((class, number), &sym_id) in &lowering.register_symbols {
                    let sym_lit = proc_macro2::Literal::usize_unsuffixed(sym_id as usize);
                    let class_lit = proc_macro2::Literal::string(class);
                    let number_lit = proc_macro2::Literal::u16_unsuffixed(*number as u16);
                    sym_init_steps.push(quote! {
                        __syms[#sym_lit] = Some(tir::sem_expr2::Value::Int(machine.read_register(#class_lit, #number_lit)?));
                    });
                }

                quote! {
                    let mut __g = tir::sem_expr2::ExprPostGraph::new();
                    tir::sem_expr2::AsSemExpr::convert(self, &mut __g);
                    let mut __syms: Vec<Option<tir::sem_expr2::Value>> = vec![None; #max_sym_id_lit + 1];
                    #(#sym_init_steps)*
                    let __syms: Vec<tir::sem_expr2::Value> = __syms.into_iter()
                        .map(|v| v.unwrap_or_else(|| tir::sem_expr2::Value::Int(tir::utils::APInt::new(64, 0))))
                        .collect();
                    let value = match tir::sem_expr2::execute(&__g, &__syms) {
                        tir::sem_expr2::Value::Int(i) => i,
                        tir::sem_expr2::Value::Float(_) => {
                            return Err(tir_be_common::SimTrap::InvalidInstruction {
                                op: #mnemonic_lit,
                                reason: "instruction semantic expression did not evaluate to integer".to_string(),
                            });
                        }
                    };
                    #dst_write
                    Ok(())
                }
            } else {
                quote! {
                    Err(tir_be_common::SimTrap::InvalidInstruction {
                        op: #mnemonic_lit,
                        reason: "failed to convert behavior to executable expression".to_string(),
                    })
                }
            }
        } else {
            quote! {
                Ok(())
            }
        };

        machine_instruction_impls.push(quote! {
            impl tir_be_common::MachineInstruction for #name_ident {
                fn mnemonic(&self) -> &'static str {
                    #mnemonic_lit
                }

                fn width_bytes(&self) -> u8 {
                    #width_bytes_lit
                }

                fn execute(
                    &self,
                    machine: &mut dyn tir_be_common::MachineContext,
                ) -> Result<(), tir_be_common::SimTrap> {
                    #execute_body
                }
            }
        });

        // Emit parser implementations based on asm template (simple template support)
        if let Some(template) = resolve_asm_template_for_instruction(inst, item_cache) {
            let actions = compile_asm_template(&template);

            let mut parse_steps: Vec<proc_macro2::TokenStream> = Vec::new();
            for act in actions {
                match act {
                    AsmAction::Comma => {
                        parse_steps.push(quote! {
                            parser
                                .expect_symbol(tir::parse::tokens::Symbol::Comma)
                                .map_err(|_| ())?;
                        });
                    }
                    AsmAction::Operand(op_name) => {
                        if let Some(ty) = ops_map.get(&op_name) {
                            let op_name_lit = proc_macro2::Literal::string(&op_name);
                            match ty {
                                Type::Struct(class_name) => {
                                    let fn_ident =
                                        format_ident!("parse_{}", class_name.to_lowercase());
                                    let class_lit = proc_macro2::Literal::string(class_name);
                                    parse_steps.push(quote! {
                                        let idx = #fn_ident(parser)?;
                                        op_builder = op_builder.attr(
                                            #op_name_lit,
                                            tir::attributes::AttributeValue::Register(
                                                tir::attributes::RegisterAttr::Physical {
                                                    class: #class_lit.to_string(),
                                                    index: idx,
                                                },
                                            ),
                                        );
                                    });
                                }
                                Type::Integer | Type::Bits(_) => {
                                    parse_steps.push(quote! {
                                        let val: i64 = if let Some(tok) = parser.peek() {
                                            match tok {
                                                tir_be_common::Token::DecNumber(n) => {
                                                    let parsed = (*n).parse::<i64>().map_err(|_| ())?;
                                                    let _ = parser.bump();
                                                    parsed
                                                }
                                                tir_be_common::Token::HexNumber(h) => {
                                                    let s = *h;
                                                    let neg = s.starts_with('-');
                                                    let s = if neg { &s[1..] } else { s };
                                                    let s = if s.starts_with("0x") || s.starts_with("0X") { &s[2..] } else { s };
                                                    let v = i128::from_str_radix(s, 16).map_err(|_| ())?;
                                                    let v = if neg { -v } else { v };
                                                    let v_i64: i64 = v.try_into().map_err(|_| ())?;
                                                    let _ = parser.bump();
                                                    v_i64
                                                }
                                                _ => { return Err(()); }
                                            }
                                        } else { return Err(()); };
                                        op_builder = op_builder.attr(
                                            #op_name_lit,
                                            tir::attributes::AttributeValue::Int(val),
                                        );
                                    });
                                }
                                Type::String => {
                                    // Strings in asm templates aren't currently used as operands; skip for now.
                                    parse_steps.push(quote! { let _ = parser.peek(); });
                                }
                                _ => {}
                            }
                        }
                    }
                    AsmAction::Skip => {
                        parse_steps.push(quote! {});
                    }
                    AsmAction::SkipMnemonic => {
                        parse_steps.push(quote! {});
                    }
                    AsmAction::LParen => {
                        parse_steps.push(quote! {
                            match parser.bump() {
                                Some(tir_be_common::Token::LParen) => {}
                                _ => return Err(()),
                            }
                        });
                    }
                    AsmAction::RParen => {
                        parse_steps.push(quote! {
                            match parser.bump() {
                                Some(tir_be_common::Token::RParen) => {}
                                _ => return Err(()),
                            }
                        });
                    }
                }
            }

            let parse_fn_ident = format_ident!("parse_{}_inst", &inst.name.to_lowercase());
            instruction_parsers_impls.push(quote! {
                fn #parse_fn_ident<'src>(
                    context: &tir::Context,
                    builder: &mut tir::IRBuilder,
                    parser: &mut tir::parse::tokens::Parser<'src, tir_be_common::Token<'src>>,
                ) -> Result<(), ()> {
                    let mut op_builder = #builder_ident::new(context);
                    #(#parse_steps)*
                    let op = op_builder.build();
                    builder.insert(op);
                    Ok(())
                }
            });

            if let Some(mn) = mnemonic.as_deref().or(Some(op_name)) {
                let mn_lit = proc_macro2::Literal::string(mn);
                instruction_parser_map_inits.push(quote! {
                    let f: tir_be_common::AsmInstructionParser = #parse_fn_ident;
                    map.insert(#mn_lit.to_string(), Box::new(f));
                });
            }
        }
    }

    Ok(quote! {
        #(#instruction_defs)*
        #(#instruction_custom_format_impls)*
        #(#machine_instruction_impls)*
        #(#as_sem_expr_impls)*

        fn get_instruction_parsers() -> std::collections::HashMap<String, Box<tir_be_common::AsmInstructionParser>> {
            let mut map = std::collections::HashMap::new();
            #(#instruction_parsers_impls)*
            #(#instruction_parser_map_inits)*

            map
        }

        #(#isel_rule_emitters)*

        pub fn get_isel_rules() -> Vec<tir_be_common::isel::Rule> {
            let mut rules = Vec::new();
            #(#isel_rule_inits)*
            rules
        }
    })
}

fn emit_register_parsers_and_printers(
    files: &[ast::File],
) -> Result<proc_macro2::TokenStream, TMDLError> {
    let mut fns = Vec::new();

    for rc in files.iter().flat_map(|f| f.register_classes()) {
        let rc_name = &rc.name;
        let fn_name = format_ident!("parse_{}", rc_name.to_lowercase());
        let print_fn_name = format_ident!("print_{}", rc_name.to_lowercase());
        let tables = rc.register_name_tables();

        let match_arms = tables
            .parse_names
            .iter()
            .map(|(name, idx)| quote! { #name => Some(#idx as u16), })
            .collect::<Vec<_>>();
        let abi_match_arms = tables
            .abi_names
            .iter()
            .map(|(idx, name)| {
                let idx_lit = proc_macro2::Literal::u16_unsuffixed(*idx);
                quote! { #idx_lit => return Some(#name.to_string()), }
            })
            .collect::<Vec<_>>();
        let isa_match_arms = tables
            .isa_names
            .iter()
            .map(|(idx, name)| {
                let idx_lit = proc_macro2::Literal::u16_unsuffixed(*idx);
                quote! { #idx_lit => return Some(#name.to_string()), }
            })
            .collect::<Vec<_>>();

        fns.push(quote! {
            fn #fn_name<'src>(parser: &mut tir::parse::tokens::Parser<'src, tir_be_common::Token<'src>>) -> Result<u16, ()> {
                if let Some(name) = parser.parse_ident() {
                    let idx = match name {
                        #(#match_arms)*
                        _ => None,
                    };
                    if let Some(i) = idx { return Ok(i); }
                }
                Err(())
            }
            fn #print_fn_name(idx: u16, prefer_abi: bool) -> Option<String> {
                if prefer_abi {
                    match idx { #(#abi_match_arms)* _ => {} }
                }
                match idx { #(#isa_match_arms)* _ => None }
            }
        });
    }

    Ok(quote! { #(#fns)* })
}

fn emit_register_trait_helpers(files: &[ast::File]) -> Result<proc_macro2::TokenStream, TMDLError> {
    let mut hardwired_arms = Vec::new();

    for rc in files.iter().flat_map(|f| f.register_classes()) {
        let class_lit = proc_macro2::Literal::string(&rc.name);
        if let Some(idx) = rc.hardwired_zero_register_index() {
            let idx_lit = proc_macro2::Literal::u16_unsuffixed(idx);
            hardwired_arms.push(quote! { (#class_lit, #idx_lit) => true, });
        }
    }

    Ok(quote! {
        pub fn register_has_trait_hardwired_zero(class: &str, index: u16) -> bool {
            match (class, index) {
                #(#hardwired_arms)*
                _ => false,
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Instruction analysis helpers
// ---------------------------------------------------------------------------

struct InstructionSemantics {
    pattern: tir::sem_expr2::ExprPostGraph,
    root: tir::graph::NodeId,
    base_cost: u32,
    variable_symbols: HashMap<String, u32>,
    fixed_register_by_class: HashMap<String, Option<u16>>,
}

fn analyze_instruction_semantics(
    inst: &ast::Instruction,
    operands: &[(String, Type)],
    defined_register_operands: &[String],
    numeric_params: &HashMap<String, i64>,
) -> Option<InstructionSemantics> {
    let rhs = resolve_behavior_rhs(inst, operands, defined_register_operands)?;
    let mut pattern = tir::sem_expr2::ExprPostGraph::new();
    let lowering = rhs.lower_to_sema(&mut pattern, numeric_params)?;
    let base_cost = pattern.len().try_into().unwrap_or(u32::MAX).max(1);
    let fixed_register_by_class = split_fixed_registers(&lowering.register_symbols);

    Some(InstructionSemantics {
        pattern,
        root: lowering.root,
        base_cost,
        variable_symbols: lowering.variable_symbols,
        fixed_register_by_class,
    })
}

fn split_fixed_registers(symbols: &HashMap<(String, u32), u32>) -> HashMap<String, Option<u16>> {
    let mut fixed_register_by_class: HashMap<String, Option<u16>> = HashMap::new();

    for ((class, number), _) in symbols {
        let entry = fixed_register_by_class.entry(class.clone()).or_insert(None);
        if let Ok(number_u16) = u16::try_from(*number) {
            match entry {
                None => *entry = Some(number_u16),
                Some(existing) if *existing == number_u16 => {}
                Some(_) => *entry = None,
            }
        } else {
            *entry = None;
        }
    }

    fixed_register_by_class
}

fn register_operand_names(operands: &[(String, Type)]) -> HashSet<&str> {
    operands
        .iter()
        .filter_map(|(name, ty)| match ty {
            Type::Struct(_) => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

fn assignment_dest_name(dest: &ast::Expr) -> Option<String> {
    match dest {
        ast::Expr::Ident(id) => Some(id.name.clone()),
        ast::Expr::Path(path) if path.remainder.len() == 1 => Some(path.remainder[0].clone()),
        _ => None,
    }
}

fn collect_behavior_assignments<'a>(expr: &'a ast::Expr, out: &mut Vec<(String, &'a ast::Expr)>) {
    match expr {
        ast::Expr::Assign(a) => {
            if let Some(dst) = assignment_dest_name(&a.dest) {
                out.push((dst, a.value.as_ref()));
            }
        }
        ast::Expr::Block(b) => {
            for stmt in &b.stmts {
                collect_behavior_assignments(stmt, out);
            }
        }
        ast::Expr::If(i) => {
            collect_behavior_assignments(i.then.as_ref(), out);
            if let Some(else_expr) = &i.else_ {
                collect_behavior_assignments(else_expr.as_ref(), out);
            }
        }
        _ => {}
    }
}

fn infer_defined_register_operands(
    behavior: &ast::Expr,
    operands: &[(String, Type)],
) -> Vec<String> {
    let register_operands = register_operand_names(operands);

    let mut defs = Vec::new();
    let mut assignments = Vec::new();
    collect_behavior_assignments(behavior, &mut assignments);
    for (dst, _) in assignments {
        if register_operands.contains(dst.as_str()) && !defs.iter().any(|existing| existing == &dst)
        {
            defs.push(dst);
        }
    }
    defs
}

fn resolve_behavior_rhs<'a>(
    inst: &'a ast::Instruction,
    operands: &[(String, Type)],
    defined_register_operands: &[String],
) -> Option<&'a ast::Expr> {
    let register_operands = register_operand_names(operands);

    let mut assignments = Vec::new();
    collect_behavior_assignments(&inst.behavior, &mut assignments);
    for (dst, rhs) in assignments.iter().rev() {
        if defined_register_operands.iter().any(|d| d == dst) {
            return Some(*rhs);
        }
    }
    for (dst, rhs) in assignments.iter().rev() {
        if register_operands.contains(dst.as_str()) {
            return Some(*rhs);
        }
    }
    match &inst.behavior {
        ast::Expr::Assign(a) => Some(a.value.as_ref()),
        ast::Expr::Block(_) | ast::Expr::If(_) => None,
        other => Some(other),
    }
}

// ---------------------------------------------------------------------------
// Template / asm helpers
// ---------------------------------------------------------------------------

fn resolve_string(expr: &ast::Expr) -> Option<String> {
    match &expr {
        ast::Expr::Lit(lit) => match lit {
            ast::Lit::Str(lstr) => Some(lstr.value().to_owned()),
            _ => None,
        },
        ast::Expr::Block(b) => {
            if b.last_expr_return {
                if let Some(ast::Expr::Lit(ast::Lit::Str(s))) = b.stmts.last() {
                    return Some(s.value().to_owned());
                }
            }
            None
        }
        _ => None,
    }
}

fn resolve_asm_template_for_instruction<'a>(
    inst: &'a ast::Instruction,
    item_cache: &HashMap<&'a str, &'a ast::Item>,
) -> Option<String> {
    resolve_effective_asm_for_instruction(inst, item_cache).and_then(resolve_string)
}

// Actions derived from a simple asm template string.
enum AsmAction {
    SkipMnemonic,
    Comma,
    Operand(String),
    Skip,
    LParen,
    RParen,
}

fn compile_asm_template(template: &str) -> Vec<AsmAction> {
    let mut actions = Vec::new();
    let mut i = 0;
    let bytes = template.as_bytes();
    while i < bytes.len() {
        match bytes[i] as char {
            '{' => {
                if let Some(end) = template[i + 1..].find('}') {
                    let content = &template[i + 1..i + 1 + end];
                    i = i + 1 + end + 1;
                    if content.starts_with("self.") {
                        if content.ends_with("MNEMONIC") {
                            actions.push(AsmAction::SkipMnemonic);
                        } else {
                            actions.push(AsmAction::Skip);
                        }
                    } else {
                        actions.push(AsmAction::Operand(content.to_string()));
                    }
                    continue;
                } else {
                    i += 1;
                    continue;
                }
            }
            ',' => {
                actions.push(AsmAction::Comma);
                i += 1;
            }
            '(' => {
                actions.push(AsmAction::LParen);
                i += 1;
            }
            ')' => {
                actions.push(AsmAction::RParen);
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    actions
}

// ---------------------------------------------------------------------------
// AsSemExpr code generation
// ---------------------------------------------------------------------------

fn emit_as_sem_expr_impl(
    inst: &ast::Instruction,
    operands: &[(String, Type)],
    defined_register_operands: &[String],
    name_ident: &proc_macro2::Ident,
    numeric_params: &HashMap<String, i64>,
) -> Option<proc_macro2::TokenStream> {
    let rhs = resolve_behavior_rhs(inst, operands, defined_register_operands)?;
    let mut dag = tir::sem_expr2::ExprPostGraph::new();
    let lowering = rhs.lower_to_sema(&mut dag, numeric_params)?;
    let (stmts, root_var) = emit_dag_as_code(&dag, lowering.root);

    Some(quote! {
        impl tir::sem_expr2::AsSemExpr for #name_ident {
            fn convert(
                &self,
                g: &mut impl tir::graph::MutDag<Node = tir::sem_expr2::ExprKind, Leaf = tir::sem_expr2::ExprPayload>,
            ) -> tir::graph::NodeId {
                #(#stmts)*
                #root_var
            }
        }
    })
}

fn emit_dag_as_code(
    dag: &tir::sem_expr2::ExprPostGraph,
    root: tir::graph::NodeId,
) -> (Vec<proc_macro2::TokenStream>, proc_macro2::Ident) {
    use tir::graph::Dag;

    let mut stmts: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut node_vars: HashMap<usize, proc_macro2::Ident> = HashMap::new();
    let mut counter = 0usize;

    for node_id in dag.postorder(root) {
        let var = format_ident!("__sem2_{}", counter);
        counter += 1;

        let kind_ts = emit_expr_kind_ts(dag.get_node(node_id));
        stmts.push(quote! { let #var = g.add_node(#kind_ts); });

        if let Some(data) = dag.get_leaf_data(node_id) {
            let data_ts = emit_expr_payload_ts(data);
            stmts.push(quote! { g.set_leaf_data(#var, #data_ts); });
        }

        let children: Vec<tir::graph::NodeId> = dag.children(node_id).collect();
        for child_id in children {
            let child_var = node_vars[&child_id.index()].clone();
            stmts.push(quote! { g.add_edge(#var, #child_var); });
        }

        node_vars.insert(node_id.index(), var);
    }

    let root_var = node_vars[&root.index()].clone();
    (stmts, root_var)
}

fn emit_expr_kind_ts(kind: &tir::sem_expr2::ExprKind) -> proc_macro2::TokenStream {
    use tir::sem_expr2::ExprKind;
    match kind {
        ExprKind::Symbol => quote! { tir::sem_expr2::ExprKind::Symbol },
        ExprKind::Constant => quote! { tir::sem_expr2::ExprKind::Constant },
        ExprKind::Add => quote! { tir::sem_expr2::ExprKind::Add },
        ExprKind::Sub => quote! { tir::sem_expr2::ExprKind::Sub },
        ExprKind::Mul => quote! { tir::sem_expr2::ExprKind::Mul },
        ExprKind::Div => quote! { tir::sem_expr2::ExprKind::Div },
        ExprKind::UDiv => quote! { tir::sem_expr2::ExprKind::UDiv },
        ExprKind::Eq => quote! { tir::sem_expr2::ExprKind::Eq },
        ExprKind::Ne => quote! { tir::sem_expr2::ExprKind::Ne },
        ExprKind::Lt => quote! { tir::sem_expr2::ExprKind::Lt },
        ExprKind::Gt => quote! { tir::sem_expr2::ExprKind::Gt },
        ExprKind::Ge => quote! { tir::sem_expr2::ExprKind::Ge },
        ExprKind::ULt => quote! { tir::sem_expr2::ExprKind::ULt },
        ExprKind::ULe => quote! { tir::sem_expr2::ExprKind::ULe },
        ExprKind::UGt => quote! { tir::sem_expr2::ExprKind::UGt },
        ExprKind::UGe => quote! { tir::sem_expr2::ExprKind::UGe },
        ExprKind::ShiftLeft => quote! { tir::sem_expr2::ExprKind::ShiftLeft },
        ExprKind::ShiftRightArithmetic => quote! { tir::sem_expr2::ExprKind::ShiftRightArithmetic },
        ExprKind::ShiftRightLogic => quote! { tir::sem_expr2::ExprKind::ShiftRightLogic },
        ExprKind::Or => quote! { tir::sem_expr2::ExprKind::Or },
        ExprKind::And => quote! { tir::sem_expr2::ExprKind::And },
        ExprKind::Xor => quote! { tir::sem_expr2::ExprKind::Xor },
        ExprKind::If => quote! { tir::sem_expr2::ExprKind::If },
        ExprKind::Clamp => quote! { tir::sem_expr2::ExprKind::Clamp },
        ExprKind::LoadMemory => quote! { tir::sem_expr2::ExprKind::LoadMemory },
        ExprKind::StoreMemory => quote! { tir::sem_expr2::ExprKind::StoreMemory },
        ExprKind::ZExt => quote! { tir::sem_expr2::ExprKind::ZExt },
        ExprKind::SExt => quote! { tir::sem_expr2::ExprKind::SExt },
        ExprKind::Log2Ceil => quote! { tir::sem_expr2::ExprKind::Log2Ceil },
        ExprKind::Sqrt => quote! { tir::sem_expr2::ExprKind::Sqrt },
        ExprKind::Fma => quote! { tir::sem_expr2::ExprKind::Fma },
    }
}

fn emit_expr_payload_ts(payload: &tir::sem_expr2::ExprPayload) -> proc_macro2::TokenStream {
    use tir::sem_expr2::ExprPayload;
    match payload {
        ExprPayload::SymbolId(id) => {
            let id_lit = proc_macro2::Literal::u32_unsuffixed(*id);
            quote! { tir::sem_expr2::ExprPayload::SymbolId(#id_lit) }
        }
        ExprPayload::Int(v) => {
            let width = proc_macro2::Literal::u32_unsuffixed(v.width());
            if v.is_signed() {
                let val = proc_macro2::Literal::i64_unsuffixed(v.to_i64());
                quote! { tir::sem_expr2::ExprPayload::Int(tir::utils::APInt::new_signed(#width, #val)) }
            } else {
                let val = proc_macro2::Literal::u64_unsuffixed(v.to_u64());
                quote! { tir::sem_expr2::ExprPayload::Int(tir::utils::APInt::new(#width, #val)) }
            }
        }
        ExprPayload::Float(f) => {
            let val = proc_macro2::Literal::f64_unsuffixed(f.to_f64());
            quote! { tir::sem_expr2::ExprPayload::Float(tir::utils::APFloat::from_f64(#val)) }
        }
    }
}
