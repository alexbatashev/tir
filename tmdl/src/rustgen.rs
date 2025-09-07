use std::collections::HashMap;
use std::io::Write;

use quote::{format_ident, quote};

use crate::ast;
use crate::error::TMDLError;

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

    let registers = emit_register_parsers(&ast)?;
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
        instruction_defs.push(quote! {
            operation! {
                #name_ident {
                    name: #mnemonic,
                    dialect: #dialect,
                }
            }
        });

        if let Some(mn) = &mnemonic {
            if let Some(asm_str) = resolve_asm_for_instruction(inst, item_cache) {
                // Decide operand parse sequence by scanning placeholders in asm string
                let (placeholders, has_commas) = parse_asm_placeholders(&asm_str);
                let operands = resolve_operands_for_instruction(inst, item_cache);

                let mut parser_body: Vec<proc_macro2::TokenStream> = vec![];

                let mut first = true;
                for ph in placeholders {
                    if ph == "self.MNEMONIC" {
                        // mnemonic is already consumed by dispatcher
                        continue;
                    }

                    // Insert comma expectation if commas are present and not first operand
                    if has_commas && !first {
                        parser_body.push(quote! {
                            parser.expect_symbol(tir::parse::tokens::Symbol::Comma).map_err(|_| ())?;
                        });
                    }
                    first = false;

                    // Determine operand type and generate appropriate parser step
                    if let Some(ty) = operands.get(&ph) {
                        match ty {
                            ast::Type::Struct(s) => {
                                let reg_fn = format_ident!("parse_{}", s);
                                parser_body.push(quote! { let _ = #reg_fn(parser)?; });
                            }
                            ast::Type::Bits(_) | ast::Type::Integer => {
                                parser_body.push(quote! {
                                    match parser.peek() {
                                        Some(tir_be_common::Token::DecNumber(_)) => { let _ = parser.bump(); },
                                        _ => return Err(())
                                    }
                                });
                            }
                            ast::Type::String => {
                                parser_body.push(quote! { return Err(()); });
                            }
                        }
                    }
                }

                let func_ident = format_ident!("parse_{}", mn);
                instruction_parsers_impls.push(quote! {
                    fn #func_ident<'src>(
                        context: &tir::Context,
                        builder: &mut tir::IRBuilder,
                        parser: &mut tir::parse::tokens::Parser<'src, tir_be_common::Token<'src>>,
                    ) -> Result<(), ()> {
                        // Insert the instruction op
                        let _ = builder.insert(#builder_ident::new(context).build());
                        // Parse operands according to ASM template
                        #(#parser_body)*
                        Ok(())
                    }
                });

                let mnemonic_str = mn.clone();
                instruction_parser_map_inits.push(quote! {
                    map.insert(#mnemonic_str.to_string(), Box::new(#func_ident as tir_be_common::AsmInstructionParser));
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

fn resolve_asm_for_instruction<'a>(
    inst: &'a ast::Instruction,
    item_cache: &HashMap<String, &'a ast::Item>,
) -> Option<String> {
    if let Some(a) = &inst.asm {
        if let Some(s) = resolve_string(a) {
            return Some(s);
        }
    }
    let mut parent = inst.parent_template.clone();
    while let Some(p) = parent {
        match item_cache.get(&p) {
            Some(ast::Item::Template(t)) => {
                if let Some(a) = &t.asm {
                    if let Some(s) = resolve_string(a) {
                        return Some(s);
                    }
                }
                parent = t.parent_template.clone();
            }
            _ => break,
        }
    }
    None
}

fn resolve_operands_for_instruction<'a>(
    inst: &'a ast::Instruction,
    item_cache: &HashMap<String, &'a ast::Item>,
) -> HashMap<String, ast::Type> {
    let mut result = HashMap::new();

    // collect from root-most template first
    fn collect_from_template<'a>(
        name: &str,
        cache: &HashMap<String, &'a ast::Item>,
        acc: &mut HashMap<String, ast::Type>,
    ) {
        if let Some(ast::Item::Template(t)) = cache.get(name) {
            if let Some(parent) = &t.parent_template {
                collect_from_template(parent, cache, acc);
            }
            for (k, v) in &t.operands {
                acc.insert(k.clone(), v.clone());
            }
        }
    }

    if let Some(p) = &inst.parent_template {
        collect_from_template(p, item_cache, &mut result);
    }
    for (k, v) in &inst.operands {
        result.insert(k.clone(), v.clone());
    }
    result
}

fn parse_asm_placeholders(asm: &str) -> (Vec<String>, bool) {
    let mut names = Vec::new();
    let mut i = 0usize;
    let bytes = asm.as_bytes();
    let mut has_commas = false;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // find matching }
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'}' {
                let name = &asm[i + 1..j];
                names.push(name.to_string());
                i = j + 1;
                continue;
            } else {
                break;
            }
        } else if bytes[i] == b',' {
            has_commas = true;
        }
        i += 1;
    }
    (names, has_commas)
}

fn emit_register_parsers(ast: &[ast::File]) -> Result<proc_macro2::TokenStream, TMDLError> {
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

        for (idx, isa_name, alias) in entries {
            if idx != u16::MAX {
                let lit_isa = isa_name.clone();
                match_arms.push(quote! { #lit_isa => Some(#idx as u16), });
            }
            if let Some(a) = alias {
                if let Some(stem) = alias_stem(&a) {
                    let counter = next_alias_index.entry(stem.clone()).or_insert(0);
                    let alias_full = format!("{}{}", stem, *counter);
                    *counter += 1;
                    match_arms.push(quote! { #alias_full => Some(#idx as u16), });
                } else {
                    // fixed alias (like "zero", "ra")
                    match_arms.push(quote! { #a => Some(#idx as u16), });
                }
            }
        }

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
