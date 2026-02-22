use std::collections::{HashMap, HashSet};
use std::io::Write;

use quote::{format_ident, quote};

use crate::Type;
use crate::ast;
use crate::error::TMDLError;
use crate::utils::{
    get_encoding_arms, parse_literal_value, resolve_effective_asm_for_instruction,
    resolve_operands_for_instruction, resolve_params_for_instruction,
};

struct InstructionSemantics {
    pattern: proc_macro2::TokenStream,
    base_cost: u32,
    variable_symbols: HashMap<String, u32>,
    fixed_register_by_class: HashMap<String, Option<u16>>,
}

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

    for inst in files.iter().flat_map(|f| f.instructions()) {
        let name_ident = format_ident!("{}Op", &inst.name);
        let builder_ident = format_ident!("{}OpBuilder", &inst.name);
        let mnemonic = resolve_string(inst.params.get("MNEMONIC").unwrap().1.as_ref().unwrap());
        let fallback_name = inst.name.to_lowercase();
        let mnemonic_name = mnemonic.as_deref().unwrap_or(&fallback_name);
        let mnemonic_lit = proc_macro2::Literal::string(mnemonic.as_deref().unwrap_or(""));
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
                    name: #mnemonic_lit,
                    dialect: #dialect,
                    attributes: A { #attrs_schema },
                    roles: R { #roles_schema },
                    interfaces: [tir_be_common::MachineInstruction],
                    format: custom,
                }
            }
        });

        let op_display_name = format!("{}.{}", dialect, mnemonic_name);
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

        if let Some(semantics) =
            analyze_instruction_semantics(inst, item_cache, &ops, &defined_register_operands)
        {
            let emit_fn_ident = format_ident!("emit_isel_{}", inst.name.to_lowercase());
            let rule_name_lit = proc_macro2::Literal::string(&inst.name.to_lowercase());
            let mut emit_attr_steps = Vec::new();
            for (op_name, op_ty) in &ops {
                let op_name_lit = proc_macro2::Literal::string(&op_name);
                match op_ty {
                    Type::Struct(class_name) => {
                        let class_lit = proc_macro2::Literal::string(&class_name);
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

            let pattern = semantics.pattern;
            let base_cost_lit = proc_macro2::Literal::u32_unsuffixed(semantics.base_cost);
            isel_rule_emitters.push(quote! {
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
                    #pattern,
                    #base_cost_lit,
                    #emit_fn_ident,
                ));
            });
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
        let op_name_lit = proc_macro2::Literal::string(mnemonic_name);

        let execute_body = if let Some(rhs) =
            resolve_behavior_rhs(inst, &ops, &defined_register_operands)
        {
            let numeric_params: HashMap<_, _> = resolve_params_for_instruction(inst, item_cache)
                .into_iter()
                .filter_map(|(name, (_ty, value))| match value {
                    Some(ast::Expr::Lit(ast::Lit::Int(li))) => {
                        Some((name, parse_literal_value(&li) as i64))
                    }
                    _ => None,
                })
                .collect();

            if let Ok(converted) = crate::sem_expr_conv::convert_to_sem_expr(rhs, numeric_params) {
                let expr_tokens = emit_sem_expr(&converted.expr);
                let mut symbol_arms = Vec::new();
                for (symbol_id, info) in converted.symbols {
                    let sym_lit = proc_macro2::Literal::u32_unsuffixed(symbol_id);
                    match info {
                        crate::sem_expr_conv::SymbolInfo::Variable { name } => {
                            let name_lit = proc_macro2::Literal::string(&name);
                            if let Some((_, ty)) = ops.iter().find(|(n, _)| n == &name) {
                                match ty {
                                    Type::Struct(_) => {
                                        symbol_arms.push(quote! {
                                            #sym_lit => {
                                                let (class, index) = tir_be_common::register_attr(self.attributes(), #name_lit)
                                                    .ok_or(tir_be_common::SimTrap::MissingAttribute {
                                                        op: #op_name_lit,
                                                        attribute: #name_lit,
                                                    })?;
                                                Ok(Some(machine.read_register(&class, index)?))
                                            }
                                        });
                                    }
                                    Type::Integer => {
                                        symbol_arms.push(quote! {
                                            #sym_lit => {
                                                let value = tir_be_common::int_attr(self.attributes(), #name_lit).ok_or(
                                                    tir_be_common::SimTrap::MissingAttribute {
                                                        op: #op_name_lit,
                                                        attribute: #name_lit,
                                                    },
                                                )?;
                                                Ok(Some(tir::sem_expr::APInt::new_signed(64, value)))
                                            }
                                        });
                                    }
                                    Type::Bits(width) => {
                                        let width_lit =
                                            proc_macro2::Literal::u32_unsuffixed(*width as u32);
                                        symbol_arms.push(quote! {
                                            #sym_lit => {
                                                let value = tir_be_common::int_attr(self.attributes(), #name_lit).ok_or(
                                                    tir_be_common::SimTrap::MissingAttribute {
                                                        op: #op_name_lit,
                                                        attribute: #name_lit,
                                                    },
                                                )?;
                                                Ok(Some(tir::sem_expr::APInt::new_signed(#width_lit, value)))
                                            }
                                        });
                                    }
                                    Type::String => {}
                                    _ => {}
                                }
                            }
                        }
                        crate::sem_expr_conv::SymbolInfo::Register { class, number } => {
                            let class_lit = proc_macro2::Literal::string(&class);
                            let number_lit = proc_macro2::Literal::u16_unsuffixed(number as u16);
                            symbol_arms.push(quote! {
                                #sym_lit => Ok(Some(machine.read_register(#class_lit, #number_lit)?))
                            });
                        }
                    }
                }

                let dst_write = if let Some(dst_name) = defined_register_operands.last() {
                    let dst_lit = proc_macro2::Literal::string(dst_name);
                    quote! {
                        let (dst_class, dst_idx) = tir_be_common::register_attr(self.attributes(), #dst_lit).ok_or(
                            tir_be_common::SimTrap::MissingAttribute {
                                op: #op_name_lit,
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

                quote! {
                    let expr = #expr_tokens;
                    let resolved = tir_be_common::resolve_expr_symbols(&expr, |symbol| {
                        match symbol {
                            #(#symbol_arms,)*
                            _ => Ok(None),
                        }
                    })?;
                    let evaluated = tir::sem_expr::evaluate(resolved);
                    let value = match evaluated {
                        tir::sem_expr::Expr::Int(i) => i,
                        _ => {
                            return Err(tir_be_common::SimTrap::InvalidInstruction {
                                op: #op_name_lit,
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
                        op: #op_name_lit,
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
                    #op_name_lit
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

            if let Some(mn) = &mnemonic {
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

fn analyze_instruction_semantics<'a>(
    inst: &'a ast::Instruction,
    item_cache: &HashMap<&'a str, &'a ast::Item>,
    operands: &[(String, Type)],
    defined_register_operands: &[String],
) -> Option<InstructionSemantics> {
    let rhs = resolve_behavior_rhs(inst, operands, defined_register_operands)?;

    let numeric_params: HashMap<_, _> = resolve_params_for_instruction(inst, item_cache)
        .into_iter()
        .filter_map(|(name, (_ty, value))| match value {
            Some(ast::Expr::Lit(ast::Lit::Int(li))) => {
                Some((name, parse_literal_value(&li) as i64))
            }
            _ => None,
        })
        .collect();

    let converted = crate::sem_expr_conv::convert_to_sem_expr(rhs, numeric_params).ok()?;
    let pattern = emit_sem_expr(&converted.expr);
    let base_cost = sem_expr_complexity(&converted.expr).max(1);
    let (variable_symbols, fixed_register_by_class) = split_symbols(&converted.symbols);

    Some(InstructionSemantics {
        pattern,
        base_cost,
        variable_symbols,
        fixed_register_by_class,
    })
}

fn split_symbols(
    symbols: &HashMap<u32, crate::sem_expr_conv::SymbolInfo>,
) -> (HashMap<String, u32>, HashMap<String, Option<u16>>) {
    let mut variable_symbols: HashMap<String, u32> = HashMap::new();
    let mut fixed_register_by_class: HashMap<String, Option<u16>> = HashMap::new();

    for (sym, info) in symbols {
        match info {
            crate::sem_expr_conv::SymbolInfo::Variable { name } => {
                variable_symbols.insert(name.clone(), *sym);
            }
            crate::sem_expr_conv::SymbolInfo::Register { class, number } => {
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
        }
    }

    (variable_symbols, fixed_register_by_class)
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

fn collect_behavior_assignments<'a>(expr: &'a ast::Expr, out: &mut Vec<(&'a str, &'a ast::Expr)>) {
    match expr {
        ast::Expr::Assign(a) => out.push((a.dest.as_str(), a.value.as_ref())),
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
        if register_operands.contains(dst) && !defs.iter().any(|existing| existing == dst) {
            defs.push(dst.to_string());
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
        if register_operands.contains(*dst) {
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
// sem_expr code emission
// ---------------------------------------------------------------------------

fn emit_sem_expr(expr: &tir::sem_expr::Expr) -> proc_macro2::TokenStream {
    use tir::sem_expr::Expr;
    match expr {
        Expr::Int(v) => {
            let width = proc_macro2::Literal::u32_unsuffixed(v.width());
            if v.is_signed() {
                let value = proc_macro2::Literal::i64_unsuffixed(v.to_i64());
                quote! { tir::sem_expr::Expr::Int(tir::sem_expr::APInt::new_signed(#width, #value)) }
            } else {
                let value = proc_macro2::Literal::u64_unsuffixed(v.to_u64());
                quote! { tir::sem_expr::Expr::Int(tir::sem_expr::APInt::new(#width, #value)) }
            }
        }
        Expr::Bool(v) => {
            quote! { tir::sem_expr::Expr::Bool(#v) }
        }
        Expr::Symbol(id) => {
            let id_lit = proc_macro2::Literal::u32_unsuffixed(*id);
            quote! { tir::sem_expr::Expr::Symbol(#id_lit) }
        }
        Expr::Add(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Add(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Sub(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Sub(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Mul(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Mul(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Div(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Div(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::UDiv(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::UDiv(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Eq(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Eq(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Ne(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Ne(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Lt(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Lt(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Le(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Le(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Gt(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Gt(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Ge(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Ge(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::ULt(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::ULt(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::ULe(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::ULe(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::UGt(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::UGt(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::UGe(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::UGe(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::ShiftLeft(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::ShiftLeft(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::ShiftRightLogic(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::ShiftRightLogic(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::ShiftRightArithmetic(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::ShiftRightArithmetic(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::And(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::And(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Or(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Or(Box::new(#lhs), Box::new(#rhs)) }
        }
        Expr::Xor(lhs, rhs) => {
            let lhs = emit_sem_expr(lhs);
            let rhs = emit_sem_expr(rhs);
            quote! { tir::sem_expr::Expr::Xor(Box::new(#lhs), Box::new(#rhs)) }
        }
        _ => quote! { tir::sem_expr::Expr::Bool(false) },
    }
}

fn sem_expr_complexity(expr: &tir::sem_expr::Expr) -> u32 {
    use tir::sem_expr::Expr;
    match expr {
        Expr::Int(_) | Expr::Bool(_) | Expr::Symbol(_) => 1,
        Expr::Add(lhs, rhs)
        | Expr::Sub(lhs, rhs)
        | Expr::Mul(lhs, rhs)
        | Expr::Div(lhs, rhs)
        | Expr::UDiv(lhs, rhs)
        | Expr::Eq(lhs, rhs)
        | Expr::Ne(lhs, rhs)
        | Expr::Lt(lhs, rhs)
        | Expr::Le(lhs, rhs)
        | Expr::Gt(lhs, rhs)
        | Expr::Ge(lhs, rhs)
        | Expr::ULt(lhs, rhs)
        | Expr::ULe(lhs, rhs)
        | Expr::UGt(lhs, rhs)
        | Expr::UGe(lhs, rhs)
        | Expr::ShiftLeft(lhs, rhs)
        | Expr::ShiftRightLogic(lhs, rhs)
        | Expr::ShiftRightArithmetic(lhs, rhs)
        | Expr::And(lhs, rhs)
        | Expr::Or(lhs, rhs)
        | Expr::Xor(lhs, rhs) => 1 + sem_expr_complexity(lhs) + sem_expr_complexity(rhs),
        _ => 2,
    }
}
