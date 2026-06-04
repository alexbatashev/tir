use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Expr, Lit, Meta, parse_macro_input};

pub fn construct_simple_node(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    // Parse #[simple_node(default_arity = N)] on the enum itself.
    let mut default_arity: usize = 2;
    for attr in &input.attrs {
        if attr.path().is_ident("simple_node") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default_arity") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Int(n) = lit {
                        default_arity = n.base10_parse()?;
                    }
                }
                Ok(())
            })
            .expect("malformed #[simple_node(...)] attribute");
        }
    }

    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => panic!("SimpleNode can only be derived for enums"),
    };

    let mut leaf_variants: Vec<&syn::Ident> = vec![];
    // BTreeMap keeps arities in sorted order for deterministic output.
    let mut arity_map: std::collections::BTreeMap<usize, Vec<&syn::Ident>> =
        std::collections::BTreeMap::new();

    for variant in variants {
        let ident = &variant.ident;
        let mut is_leaf = false;
        let mut custom_arity: Option<usize> = None;

        for attr in &variant.attrs {
            if attr.path().is_ident("leaf") {
                is_leaf = true;
            } else if attr.path().is_ident("arity") {
                // #[arity = N]
                if let Meta::NameValue(nv) = &attr.meta
                    && let Expr::Lit(expr_lit) = &nv.value
                    && let Lit::Int(n) = &expr_lit.lit
                {
                    custom_arity = Some(n.base10_parse().expect("arity must be an integer"));
                }
            }
        }

        if is_leaf {
            leaf_variants.push(ident);
        } else if let Some(a) = custom_arity {
            arity_map.entry(a).or_default().push(ident);
        }
    }

    // ── is_leaf ──────────────────────────────────────────────────────────────
    let is_leaf_body = if leaf_variants.is_empty() {
        quote! { _ => false, }
    } else {
        quote! {
            #( Self::#leaf_variants )|* => true,
            _ => false,
        }
    };

    // ── num_children ─────────────────────────────────────────────────────────
    let mut children_arms = vec![];

    if !leaf_variants.is_empty() {
        children_arms.push(quote! { #( Self::#leaf_variants )|* => 0, });
    }

    for (arity, variants) in &arity_map {
        children_arms.push(quote! { #( Self::#variants )|* => #arity, });
    }

    children_arms.push(quote! { _ => #default_arity, });

    quote! {
        impl ::tir::graph::Node for #name {
            fn is_leaf(&self, _: &::tir::Context) -> bool {
                match self {
                    #is_leaf_body
                }
            }

            fn num_children(&self, _: &::tir::Context) -> usize {
                match self {
                    #(#children_arms)*
                }
            }
        }
    }
    .into()
}
