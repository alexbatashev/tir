use std::collections::HashMap;
use std::io::Write;

use quote::{format_ident, quote};

use crate::ast;
use crate::error::TMDLError;
use crate::utils::resolve_operands_for_instruction;

pub fn generate_rust(
    dialect: &str,
    ast: Vec<ast::File>,
    mut output: Box<dyn Write>,
) -> Result<(), TMDLError> {
    let features = emit_feautres(&ast)?;

    let item_cache = {
        let mut cache = HashMap::new();
        for f in &ast {
            for item in &f.items {
                cache.insert(item.name().to_string(), item);
            }
        }

        cache
    };

    let registers = emit_register_parsers_and_printers(&ast)?;
    let instructions = emit_instructions(dialect, &ast, &item_cache)?;

    let final_rust = quote! {
        #features

        #registers

        #instructions
    };

    let syntax_tree = syn::parse2(final_rust).unwrap();
    let formatted = prettyplease::unparse(&syntax_tree);

    output.write(formatted.as_bytes())?;

    Ok(())
}

fn emit_feautres(ast: &[ast::File]) -> Result<proc_macro2::TokenStream, TMDLError> {
    let features = ast
        .iter()
        .flat_map(|file| file.items.iter())
        .filter_map(|item| match item {
            ast::Item::Isa(isa) => Some(isa),
            _ => None,
        });

    let mut enum_variants = vec![];
    let mut name_arms = vec![];

    for feature in features {
        let ident = format_ident!("{}", &feature.name);
        let name = feature.name.clone();
        enum_variants.push(quote! {
            #ident
        });

        name_arms.push(quote! {
            Self::#ident => #name
        })
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

fn emit_instructions<'ast, 'cache: 'ast>(
    dialect: &str,
    ast: &'ast [ast::File],
    item_cache: &HashMap<String, &'cache ast::Item>,
) -> Result<proc_macro2::TokenStream, TMDLError> {
    let instructions =
        ast.iter()
            .flat_map(|file| file.items.iter())
            .filter_map(|item| match item {
                ast::Item::Instruction(inst) => Some(inst),
                _ => None,
            });

    let mut instruction_defs = vec![];
    let mut instruction_parsers_impls: Vec<proc_macro2::TokenStream> = vec![];
    let mut instruction_parser_map_inits: Vec<proc_macro2::TokenStream> = vec![];

    for inst in instructions {
        let name_ident = format_ident!("{}Op", &inst.name);
        let builder_ident = format_ident!("{}OpBuilder", &inst.name);
        let mnemonic = resolve_string(inst.params.get("MNEMONIC").unwrap().1.as_ref().unwrap());
        let mnemonic_lit =
            proc_macro2::Literal::string(&mnemonic.clone().unwrap_or_else(|| "".to_string()));
        // Build attributes schema from operands
        let attrs_schema = {
            let mut items = vec![];
            let ops = resolve_operands_for_instruction(inst, item_cache);
            for (name, ty) in ops {
                let field_ident = format_ident!("{}", name);
                let ty_ts = match ty {
                    ast::Type::Struct(_) => quote! { Register },
                    ast::Type::Integer | ast::Type::Bits(_) => quote! { Integer },
                    ast::Type::String => quote! { String },
                };
                items.push(quote! { #field_ident: #ty_ts });
            }
            quote! { #(#items,)* }
        };

        // Build roles: heuristic — rd => Def for register types, rs* => Use
        let roles_schema = {
            let mut items = vec![];
            let ops = resolve_operands_for_instruction(inst, item_cache);
            for (name, ty) in ops {
                if let ast::Type::Struct(_) = ty {
                    let field_ident = format_ident!("{}", name);
                    let role = if name == "rd" {
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
                }
            }
        });

        // Emit parser implementations based on asm template (simple template support)
        if let Some(template) = resolve_asm_template_for_instruction(inst, item_cache) {
            // Compile template into a sequence of parse actions
            let actions = compile_asm_template(&template);
            let ops = resolve_operands_for_instruction(inst, item_cache);

            // Generate parsing code for each action
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
                        if let Some(ty) = ops.get(&op_name) {
                            let op_name_lit = proc_macro2::Literal::string(&op_name);
                            match ty {
                                ast::Type::Struct(class_name) => {
                                    let fn_ident = format_ident!("parse_{}", class_name);
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
                                ast::Type::Integer | ast::Type::Bits(_) => {
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
                                ast::Type::String => {
                                    // Strings in asm templates aren't currently used as operands; skip for now.
                                    parse_steps.push(quote! { let _ = parser.peek(); });
                                }
                            }
                        }
                    }
                    AsmAction::Skip => {
                        // No-op for spaces or unsupported punctuation in simple templates
                        parse_steps.push(quote! {});
                    }
                    AsmAction::SkipMnemonic => {
                        // Mnemonic already consumed by dispatcher; ensure no-op
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

            // Build the parser function for this instruction
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

            // Insert into map by mnemonic
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

        fn get_instruction_parsers() -> std::collections::HashMap<String, Box<tir_be_common::AsmInstructionParser>> {
            let mut map = std::collections::HashMap::new();
            #(#instruction_parsers_impls)*
            #(#instruction_parser_map_inits)*

            map
        }

        // Placeholder for emitters map if needed in the future
    })
}

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

// Resolve asm template for an instruction, following parent templates if needed
fn resolve_asm_template_for_instruction<'a>(
    inst: &'a ast::Instruction,
    item_cache: &HashMap<String, &'a ast::Item>,
) -> Option<String> {
    if let Some(expr) = &inst.asm {
        if let Some(s) = resolve_string(expr) {
            return Some(s);
        }
    }
    // Walk up templates
    let mut cur = inst.parent_template.as_ref();
    while let Some(name) = cur {
        if let Some(ast::Item::Template(t)) = item_cache.get(name.as_str()) {
            if let Some(expr) = &t.asm {
                if let Some(s) = resolve_string(expr) {
                    return Some(s);
                }
            }
            cur = t.parent_template.as_ref();
        } else {
            break;
        }
    }
    None
}

// Actions derived from a simple asm template string. We only support commas and operands now.
enum AsmAction {
    // Placeholder for {self.MNEMONIC}
    SkipMnemonic,
    // A comma token between operands
    Comma,
    // An operand placeholder like {rd}, {rs1}, {imm}
    Operand(String),
    // Skip anything else (spaces or unsupported punctuation)
    Skip,
    // Parentheses
    LParen,
    RParen,
}

// Very simple template compiler: extracts operand sequence and commas.
fn compile_asm_template(template: &str) -> Vec<AsmAction> {
    let mut actions = Vec::new();
    let mut i = 0;
    let bytes = template.as_bytes();
    while i < bytes.len() {
        match bytes[i] as char {
            '{' => {
                if let Some(end) = template[i + 1..].find('}') {
                    let content = &template[i + 1..i + 1 + end];
                    // Advance past '}'
                    i = i + 1 + end + 1;
                    // Handle placeholders
                    if content.starts_with("self.") {
                        // If it's the mnemonic, we skip as dispatcher consumed it
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
                    // malformed; skip
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
                // skip whitespace and other symbols for now
                i += 1;
            }
        }
    }
    actions
}

fn emit_register_parsers_and_printers(
    ast: &[ast::File],
) -> Result<proc_macro2::TokenStream, TMDLError> {
    let reg_classes = ast
        .iter()
        .flat_map(|f| f.items.iter())
        .filter_map(|it| match it {
            ast::Item::RegisterClass(rc) => Some(rc),
            _ => None,
        });

    let mut fns = Vec::new();

    for rc in reg_classes {
        let rc_name = &rc.name;
        let fn_name = format_ident!("parse_{}", rc_name);
        let print_fn_name = format_ident!("print_{}", rc_name);

        // Build mapping from textual name -> index (u16)
        // Expand ranges and assign alias numbers across ranges with same stem
        let mut entries: Vec<(u16, String, Option<String>)> = Vec::new();
        for def in &rc.registers {
            match def {
                ast::RegisterDef::Single(s) => {
                    if let Some(idx) = parse_trailing_index(&s.name) {
                        entries.push((idx, s.name.clone(), s.alias.clone()));
                    } else {
                        // no numeric id; skip for matching by index
                        entries.push((u16::MAX, s.name.clone(), s.alias.clone()));
                    }
                }
                ast::RegisterDef::Range(r) => {
                    if let (Some(s), Some(e)) =
                        (parse_trailing_index(&r.start), parse_trailing_index(&r.end))
                    {
                        for i in s..=e {
                            let isa = format!("{}{}", strip_trailing_digits(&r.start), i);
                            entries.push((i, isa, r.alias_pattern.clone()));
                        }
                    }
                }
            }
        }
        // sort by idx to ensure alias numbering continues across ranges
        entries.sort_by_key(|(i, _, _)| *i);

        let mut next_alias_index: HashMap<String, u16> = HashMap::new();
        let mut match_arms = Vec::new();
        let mut isa_names: Vec<(u16, String)> = Vec::new();
        let mut abi_names: Vec<(u16, String)> = Vec::new();

        for (idx, isa_name, alias) in entries {
            if idx != u16::MAX {
                let lit_isa = isa_name.clone();
                match_arms.push(quote! { #lit_isa => Some(#idx as u16), });
                isa_names.push((idx, lit_isa));
            }
            if let Some(a) = alias {
                if let Some(stem) = alias_stem(&a) {
                    let counter = next_alias_index.entry(stem.clone()).or_insert(0);
                    let alias_full = format!("{}{}", stem, *counter);
                    *counter += 1;
                    match_arms.push(quote! { #alias_full => Some(#idx as u16), });
                    abi_names.push((idx, alias_full));
                } else {
                    // fixed alias (like "zero", "ra")
                    match_arms.push(quote! { #a => Some(#idx as u16), });
                    abi_names.push((idx, a));
                }
            }
        }

        let abi_match_arms = {
            let mut v = Vec::new();
            for (idx, name) in &abi_names {
                let idx_lit = proc_macro2::Literal::u16_unsuffixed(*idx);
                v.push(quote! { #idx_lit => return Some(#name.to_string()), });
            }
            quote! { #(#v)* }
        };
        let isa_match_arms = {
            let mut v = Vec::new();
            for (idx, name) in &isa_names {
                let idx_lit = proc_macro2::Literal::u16_unsuffixed(*idx);
                v.push(quote! { #idx_lit => return Some(#name.to_string()), });
            }
            quote! { #(#v)* }
        };

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
                    match idx { #abi_match_arms _ => {} }
                }
                match idx { #isa_match_arms _ => None }
            }
        });
    }

    Ok(quote! { #(#fns)* })
}

fn parse_trailing_index(s: &str) -> Option<u16> {
    let mut i = s.len();
    while i > 0 && s.as_bytes()[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i < s.len() {
        s[i..].parse::<u16>().ok()
    } else {
        None
    }
}

fn strip_trailing_digits(s: &str) -> &str {
    let mut i = s.len();
    while i > 0 && s.as_bytes()[i - 1].is_ascii_digit() {
        i -= 1;
    }
    &s[..i]
}

fn alias_stem(pat: &str) -> Option<String> {
    if pat.contains("{}") {
        Some(pat.replace("{}", ""))
    } else {
        None
    }
}
