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

    let instructions = emit_instructions(dialect, &ast, &item_cache)?;

    let final_rust = quote! {
        #features

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
    let mut instruction_parsers: Vec<proc_macro2::TokenStream> = vec![];

    for inst in instructions {
        let name_ident = format_ident!("{}Op", &inst.name);
        let mnemonic = resolve_string(inst.params.get("MNEMONIC").unwrap().1.as_ref().unwrap());
        instruction_defs.push(quote! {
            operation! {
                #name_ident {
                    name: #mnemonic,
                    dialect: #dialect,
                }
            }
        });
    }

    Ok(quote! {
        #(#instruction_defs)*

        fn get_instruction_parsers() -> std::collections::HashMap<String, Box<tir_be_common::AsmInstructionParser>> {
            let mut map = std::collections::HashMap::new();
            #(#instruction_parsers)*

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
        _ => None,
    }
}
