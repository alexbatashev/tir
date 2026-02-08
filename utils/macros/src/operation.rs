use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Expr, ExprStruct, Ident, Member, parse::Parse, parse_macro_input};

use crate::utils::{expr_as_ident_vec, expr_as_string, field_name, op_fn_ident};

pub fn construct_operation(item: TokenStream) -> TokenStream {
    let Operation {
        struct_name,
        name,
        dialect,
        regions,
        attributes,
        roles,
        operands,
        results,
        custom_format,
        semantic_expr,
    } = parse_macro_input!(item as Operation);

    let builder_name = format_ident!("{}Builder", struct_name.to_string());
    let has_results = !results.is_empty();
    let op_fn_name = op_fn_ident(&name);

    let printer = if custom_format {
        make_custom_printer()
    } else {
        make_generic_printer(&dialect, &name, &operands, &regions, has_results)
    };

    let mut region_fills = vec![];
    let mut region_fields = vec![];
    let mut region_defaults = vec![];
    let mut region_builders = vec![];

    let region_accessors = make_region_accessors(&regions);

    for r in &regions {
        let name = format_ident!("{}", r.name);

        let name_str = r.name.clone();

        region_fields.push(quote! {
           #name: Option<tir::RegionId>
        });

        region_defaults.push(quote! {
           #name: None
        });

        region_builders.push(quote! {
           pub fn #name(mut self, id: tir::RegionId) -> Self {
               self.#name = Some(id);
               self
           }
        });

        if r.single_block {
            region_fills.push(quote! {
                let region = if self.#name.is_some() {
                    self.#name.unwrap()
                } else {
                    let region = self.context.create_region();
                    let block = self.context.create_block(vec![]);
                    region.add_block(block.id());
                    region.id()
                };
                regions.push(region);
            });
        } else {
            region_fills.push(quote! {
                if self.#name.is_some() {
                    regions.push(self.#name.unwrap());
                } else {
                    panic!("Region '{}' is not set", #name_str);
                }
            });
        }
    }

    let parser = if custom_format {
        make_custom_parser()
    } else {
        make_parser(&builder_name, &regions, &operands, has_results)
    };

    let verifier = make_attribute_verifier(&attributes);
    let roles_table = make_roles_table(&struct_name, &roles);

    // Operand support in builder
    let mut operand_fields = vec![];
    let mut operand_defaults = vec![];
    let mut operand_builders = vec![];
    let mut operand_fn_params = vec![];
    let mut operand_fn_builders = vec![];

    for op_name in &operands {
        let field = format_ident!("{}", op_name);
        operand_fields.push(quote! {
            #field: Option<tir::ValueId>
        });
        operand_defaults.push(quote! {
            #field: None
        });
        operand_builders.push(quote! {
            pub fn #field(mut self, v: tir::ValueId) -> Self {
                self.#field = Some(v);
                self
            }
        });
        operand_fn_params.push(quote! {
            #field: impl Into<tir::ir::Operand>
        });
        operand_fn_builders.push(quote! {
            let #field = #field.into();
            if let Some(value) = #field.into_option() {
                builder = builder.#field(value);
            }
        });
    }

    let operand_collect: Vec<_> = operands
        .iter()
        .map(|op_name| {
            let field = format_ident!("{}", op_name);
            quote! {
                if let Some(v) = self.#field {
                    operand_vec.push(v);
                }
            }
        })
        .collect();

    // Result support
    let result_accessor = if has_results {
        quote! {
            pub fn result(&self) -> tir::ValueId {
                self.0.results[0]
            }
        }
    } else {
        quote! {}
    };

    let operand_name_literals: Vec<_> = operands
        .iter()
        .map(|n| {
            let lit = proc_macro2::Literal::string(n);
            quote! { #lit }
        })
        .collect();

    let semantic_expr_method = if let Some(sem_expr) = semantic_expr {
        quote! {
            fn semantic_expr(&self) -> Option<tir::sem_expr::Expr> {
                Some(#sem_expr)
            }
        }
    } else {
        quote! {}
    };

    let result_builder_field = if has_results {
        quote! { result_type: Option<tir::Type>, }
    } else {
        quote! {}
    };

    let result_builder_default = if has_results {
        quote! { result_type: None, }
    } else {
        quote! {}
    };

    let result_builder_method = if has_results {
        quote! {
            pub fn result_type(mut self, ty: tir::Type) -> Self {
                self.result_type = Some(ty);
                self
            }
        }
    } else {
        quote! {}
    };

    let result_fn_param = if has_results {
        quote! { result_type: tir::Type, }
    } else {
        quote! {}
    };

    let result_fn_builder = if has_results {
        quote! { builder = builder.result_type(result_type); }
    } else {
        quote! {}
    };

    let attr_fn_params: Vec<_> = attributes
        .iter()
        .map(|attr| {
            let name = op_fn_ident(&attr.name);
            quote! { #name: impl Into<tir::attributes::AttributeValue> }
        })
        .collect();

    let attr_fn_builders: Vec<_> = attributes
        .iter()
        .map(|attr| {
            let name_ident = op_fn_ident(&attr.name);
            let name_str = attr.name.clone();
            quote! {
                builder = builder.attr(#name_str, #name_ident.into());
            }
        })
        .collect();

    let region_fn_params: Vec<_> = regions
        .iter()
        .map(|region| {
            let name = format_ident!("{}", region.name);
            quote! { #name: Option<tir::RegionId> }
        })
        .collect();

    let region_fn_builders: Vec<_> = regions
        .iter()
        .map(|region| {
            let name = format_ident!("{}", region.name);
            quote! {
                if let Some(region) = #name {
                    builder = builder.#name(region);
                }
            }
        })
        .collect();

    let result_build = if has_results {
        quote! {
            let result_vec = {
                let ty = self.result_type.expect("result_type must be set for ops with results");
                let val = self.context.create_value(ty, None);
                vec![val.id()]
            };
        }
    } else {
        quote! {
            let result_vec: Vec<tir::ValueId> = vec![];
        }
    };

    quote! {
        pub struct #struct_name(std::sync::Arc<tir::OpInstance>);

        pub struct #builder_name {
            context: tir::Context,
            attributes: Vec<tir::attributes::NamedAttribute>,
            #(#region_fields,)*
            #(#operand_fields,)*
            #result_builder_field
        }

        impl #struct_name {
            #region_accessors
            #roles_table
            #result_accessor
        }

        impl tir::Operation for #struct_name {
            fn name() -> &'static str
            where
                Self: Sized
            {
                #name
            }

            fn dialect() -> &'static str
            where
                Self: Sized
            {
                #dialect
            }

            fn id(&self) -> tir::OpId {
                self.0.id
            }

            fn from_op_instance(instance: std::sync::Arc<tir::OpInstance>) -> Self {
                assert_eq!(instance.name(), #name);
                #struct_name(instance)
            }

            fn from_op_instance_dyn(instance: std::sync::Arc<tir::OpInstance>) -> Box<dyn tir::Operation> {
                assert_eq!(instance.name(), #name);
                Box::new(#struct_name(instance))
            }

            fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
                self
            }

            #printer

            #parser

            fn regions(&self) -> tir::ContextIterator<tir::RegionId> {
                let context = self.0.context.upgrade();
                tir::ContextIterator::new(context, self.0.regions.clone())
            }

            fn operands(&self) -> &[tir::ValueId] {
                &self.0.operands
            }

            fn attributes(&self) -> &[tir::attributes::NamedAttribute] {
                &self.0.attributes
            }

            fn operand_names(&self) -> &'static [&'static str] {
                &[#(#operand_name_literals),*]
            }

            #semantic_expr_method
        }

        impl #builder_name {
            pub fn new(context: &tir::Context) -> #builder_name {
                Self {
                    context: context.clone(),
                    attributes: vec![],
                    #(#region_defaults,)*
                    #(#operand_defaults,)*
                    #result_builder_default
                }
            }

            #(#region_builders)*
            #(#operand_builders)*
            #result_builder_method

            pub fn attr(mut self, name: &str, value: tir::attributes::AttributeValue) -> Self {
                self.attributes.push(tir::attributes::NamedAttribute::new(name, value));
                self
            }

            pub fn build(self) -> #struct_name {
                let mut regions = vec![];

                #(#region_fills)*

                #verifier

                let mut operand_vec: Vec<tir::ValueId> = vec![];
                #(#operand_collect)*

                #result_build

                let instance = tir::OpInstance {
                    id: tir::OpId::invalid(),
                    name: #name,
                    dialect: #dialect,
                    context: self.context.as_context_ref(),
                    operands: operand_vec,
                    results: result_vec,
                    regions,
                    attributes: self.attributes,
                };

                let instance = self.context.add_operation(instance);

                #struct_name(instance)
            }
        }

        pub fn #op_fn_name(
            context: &tir::Context,
            #(#operand_fn_params,)*
            #(#attr_fn_params,)*
            #result_fn_param
            #(#region_fn_params,)*
        ) -> #builder_name {
            let mut builder = #builder_name::new(context);
            #(#operand_fn_builders)*
            #(#attr_fn_builders)*
            #result_fn_builder
            #(#region_fn_builders)*
            builder
        }
    }
    .into()
}

struct Operation {
    struct_name: Ident,
    name: String,
    dialect: String,
    regions: Vec<Region>,
    attributes: Vec<AttrSpec>,
    roles: Vec<RoleSpec>,
    operands: Vec<String>,
    results: Vec<String>,
    custom_format: bool,
    semantic_expr: Option<proc_macro2::TokenStream>,
}

struct Region {
    name: String,
    single_block: bool,
}

impl Parse for Operation {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let struct_: ExprStruct = input.parse()?;

        let struct_name = struct_.path.require_ident()?.clone();

        let name = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "name" {
                        Some(expr_as_string(&f.expr))
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap();

        let dialect = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "dialect" {
                        Some(expr_as_string(&f.expr))
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap();

        let regions = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "regions" {
                        get_regions(&f.expr)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap_or_default();

        let attributes = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "attributes" {
                        get_attributes(&f.expr)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap_or_default();

        let roles = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "roles" {
                        get_roles(&f.expr)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap_or_default();

        let operands = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "operands" {
                        Some(
                            expr_as_ident_vec(&f.expr)
                                .into_iter()
                                .map(|i| i.to_string())
                                .collect::<Vec<String>>(),
                        )
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap_or_default();

        let results = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "results" {
                        Some(
                            expr_as_ident_vec(&f.expr)
                                .into_iter()
                                .map(|i| i.to_string())
                                .collect::<Vec<String>>(),
                        )
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap_or_default();

        let custom_format = struct_
            .fields
            .iter()
            .find_map(|f| match &f.member {
                Member::Named(ident) => {
                    if ident.to_string().as_str() == "format" {
                        Some(expr_as_string(&f.expr) == "custom")
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .unwrap_or(false);

        let semantic_expr = struct_.fields.iter().find_map(|f| match &f.member {
            Member::Named(ident) => {
                if ident.to_string().as_str() == "sem" {
                    expr_as_semantic_expr(&f.expr, &operands)
                } else {
                    None
                }
            }
            _ => None,
        });

        Ok(Operation {
            struct_name,
            name,
            dialect,
            regions,
            attributes,
            roles,
            operands,
            results,
            custom_format,
            semantic_expr,
        })
    }
}

#[derive(Clone)]
enum SemNode {
    Atom(String),
    List(Vec<SemNode>),
}

fn parse_sem_expr(input: &str) -> Option<SemNode> {
    fn parse_list(chars: &[char], pos: &mut usize) -> Option<SemNode> {
        if *pos >= chars.len() || chars[*pos] != '(' {
            return None;
        }
        *pos += 1;
        let mut items = Vec::new();
        loop {
            while *pos < chars.len() && chars[*pos].is_whitespace() {
                *pos += 1;
            }
            if *pos >= chars.len() {
                return None;
            }
            if chars[*pos] == ')' {
                *pos += 1;
                break;
            }
            if chars[*pos] == '(' {
                items.push(parse_list(chars, pos)?);
                continue;
            }
            let start = *pos;
            while *pos < chars.len()
                && !chars[*pos].is_whitespace()
                && chars[*pos] != '('
                && chars[*pos] != ')'
            {
                *pos += 1;
            }
            items.push(SemNode::Atom(chars[start..*pos].iter().collect()));
        }
        Some(SemNode::List(items))
    }

    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0usize;
    while pos < chars.len() && chars[pos].is_whitespace() {
        pos += 1;
    }
    let expr = parse_list(&chars, &mut pos)?;
    while pos < chars.len() && chars[pos].is_whitespace() {
        pos += 1;
    }
    if pos == chars.len() { Some(expr) } else { None }
}

fn sem_atom_to_expr(
    atom: &str,
    operand_symbols: &std::collections::HashMap<String, u32>,
) -> Option<proc_macro2::TokenStream> {
    if let Some(sym) = operand_symbols.get(atom) {
        let sym_lit = proc_macro2::Literal::u32_unsuffixed(*sym);
        return Some(quote! { tir::sem_expr::Expr::Symbol(#sym_lit) });
    }
    if let Ok(i) = atom.parse::<i64>() {
        let width = proc_macro2::Literal::u32_unsuffixed(64);
        let value = proc_macro2::Literal::i64_unsuffixed(i);
        return Some(
            quote! { tir::sem_expr::Expr::Int(tir::sem_expr::APInt::new_signed(#width, #value)) },
        );
    }
    None
}

fn sem_node_to_expr(
    node: &SemNode,
    operand_symbols: &std::collections::HashMap<String, u32>,
) -> Option<proc_macro2::TokenStream> {
    match node {
        SemNode::Atom(a) => sem_atom_to_expr(a, operand_symbols),
        SemNode::List(items) => {
            let [SemNode::Atom(op), lhs, rhs] = items.as_slice() else {
                return None;
            };
            let lhs_ts = sem_node_to_expr(lhs, operand_symbols)?;
            let rhs_ts = sem_node_to_expr(rhs, operand_symbols)?;
            Some(match op.as_str() {
                "add" => quote! { tir::sem_expr::Expr::Add(Box::new(#lhs_ts), Box::new(#rhs_ts)) },
                "sub" => quote! { tir::sem_expr::Expr::Sub(Box::new(#lhs_ts), Box::new(#rhs_ts)) },
                "mul" => quote! { tir::sem_expr::Expr::Mul(Box::new(#lhs_ts), Box::new(#rhs_ts)) },
                "div" => quote! { tir::sem_expr::Expr::Div(Box::new(#lhs_ts), Box::new(#rhs_ts)) },
                "and" => quote! { tir::sem_expr::Expr::And(Box::new(#lhs_ts), Box::new(#rhs_ts)) },
                "or" => quote! { tir::sem_expr::Expr::Or(Box::new(#lhs_ts), Box::new(#rhs_ts)) },
                "xor" => quote! { tir::sem_expr::Expr::Xor(Box::new(#lhs_ts), Box::new(#rhs_ts)) },
                "shl" => {
                    quote! { tir::sem_expr::Expr::ShiftLeft(Box::new(#lhs_ts), Box::new(#rhs_ts)) }
                }
                "lshr" => {
                    quote! { tir::sem_expr::Expr::ShiftRightLogic(Box::new(#lhs_ts), Box::new(#rhs_ts)) }
                }
                "ashr" => {
                    quote! { tir::sem_expr::Expr::ShiftRightArithmetic(Box::new(#lhs_ts), Box::new(#rhs_ts)) }
                }
                _ => return None,
            })
        }
    }
}

fn expr_as_semantic_expr(expr: &Expr, operands: &[String]) -> Option<proc_macro2::TokenStream> {
    let sem_src = match expr {
        Expr::Lit(lit) => {
            if let syn::Lit::Str(s) = &lit.lit {
                s.value()
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let mut symbols = std::collections::HashMap::new();
    for (idx, name) in operands.iter().enumerate() {
        symbols.insert(name.clone(), idx as u32);
    }

    let parsed = parse_sem_expr(&sem_src)?;
    let SemNode::List(items) = parsed else {
        return None;
    };
    let [SemNode::Atom(set_kw), SemNode::Atom(_dst), rhs] = items.as_slice() else {
        return None;
    };
    if set_kw != "set" {
        return None;
    }

    sem_node_to_expr(rhs, &symbols)
}

fn get_regions(expr: &Expr) -> Option<Vec<Region>> {
    if let Expr::Struct(s) = expr {
        Some(
            s.fields
                .iter()
                .map(|f| {
                    let name = field_name(f);
                    Region {
                        name,
                        single_block: true,
                    }
                })
                .collect(),
        )
    } else {
        None
    }
}

#[derive(Clone)]
struct AttrSpec {
    name: String,
    ty: String,
}

fn get_attributes(expr: &Expr) -> Option<Vec<AttrSpec>> {
    if let Expr::Struct(s) = expr {
        Some(
            s.fields
                .iter()
                .map(|f| {
                    let name = field_name(f);
                    let ty = expr_as_string(&f.expr);
                    AttrSpec { name, ty }
                })
                .collect(),
        )
    } else {
        None
    }
}

fn make_attribute_verifier(specs: &[AttrSpec]) -> proc_macro2::TokenStream {
    if specs.is_empty() {
        return quote! {};
    }
    let checks = specs.iter().map(|s| {
        let n = s.name.clone();
        quote! {
            if !self.attributes.iter().any(|a| a.name == #n) {
                panic!(concat!("Missing required attribute: ", #n));
            }
        }
    });
    quote! { #(#checks)* }
}

#[derive(Clone)]
struct RoleSpec {
    name: String,
    role: String,
}

fn get_roles(expr: &Expr) -> Option<Vec<RoleSpec>> {
    if let Expr::Struct(s) = expr {
        Some(
            s.fields
                .iter()
                .map(|f| {
                    let name = field_name(f);
                    let role = expr_as_string(&f.expr);
                    RoleSpec { name, role }
                })
                .collect(),
        )
    } else {
        None
    }
}

fn make_roles_table(_op_ident: &Ident, roles: &[RoleSpec]) -> proc_macro2::TokenStream {
    if roles.is_empty() {
        return quote! {};
    }
    let mut pairs = Vec::new();
    for r in roles {
        let name = r.name.clone();
        let role_ts = match r.role.as_str() {
            "Def" => quote! { tir::attributes::AttributeRole::Def },
            "Use" => quote! { tir::attributes::AttributeRole::Use },
            "Clobber" => quote! { tir::attributes::AttributeRole::Clobber },
            "ReadWrite" => quote! { tir::attributes::AttributeRole::ReadWrite },
            _ => quote! { tir::attributes::AttributeRole::None },
        };
        pairs.push(quote! { ( #name, #role_ts ) });
    }
    let len = pairs.len();
    quote! {
        pub fn attribute_roles() -> &'static [(&'static str, tir::attributes::AttributeRole)] {
            const ROLES: [(&str, tir::attributes::AttributeRole); #len] = [ #(#pairs),* ];
            &ROLES
        }
    }
}

fn make_region_accessors(regions: &[Region]) -> proc_macro2::TokenStream {
    if !regions.is_empty() {
        if regions.len() == 1 && regions[0].single_block {
            make_sinle_block_region_accessor(&regions[0])
        } else {
            todo!()
        }
    } else {
        quote! {}
    }
}

fn make_sinle_block_region_accessor(region: &Region) -> proc_macro2::TokenStream {
    let func_name = format_ident!("{}", region.name);

    quote! {
        pub fn #func_name(&self) -> std::sync::Arc<tir::Block> {
            use tir::Operation;
            let region = self.regions().next().unwrap();
            let context = self.0.context.upgrade();
            let block = region.iter(context).next().unwrap();
            block
        }
    }
}

fn make_custom_printer() -> proc_macro2::TokenStream {
    quote! {
        fn print<'a, 'b: 'a>(&'a self, fmt: &'a mut tir::IRFormatter<'b>) -> Result<(), std::fmt::Error> {
            Self::custom_print(self, fmt)
        }
    }
}

fn make_custom_parser() -> proc_macro2::TokenStream {
    quote! {
        fn parse<'src>(parser: &mut tir::parse::text::Parser<'src>, context: &tir::Context)
        -> Result<Box<dyn tir::Operation>, (tir::parse::Span, tir::Error)> {
            Self::custom_parse(parser, context)
        }
    }
}

fn make_generic_printer(
    dialect: &str,
    name: &str,
    operands: &[String],
    regions: &[Region],
    has_results: bool,
) -> proc_macro2::TokenStream {
    let op_name = if dialect == "builtin" {
        name.to_string()
    } else {
        format!("{}.{}", dialect, name)
    };

    let result_prefix = if has_results {
        quote! {
            if !self.0.results.is_empty() {
                fmt.write(format!("%{} = ", self.0.results[0].number()))?;
            }
        }
    } else {
        quote! {}
    };

    let operand_printer = if !operands.is_empty() {
        quote! {
            if !self.0.operands.is_empty() {
                fmt.write(" ")?;
                let mut first = true;
                for op_id in &self.0.operands {
                    if !first { fmt.write(", ")?; }
                    first = false;
                    fmt.write(format!("%{}", op_id.number()))?;
                }
            }
        }
    } else {
        quote! {}
    };

    let result_suffix = if has_results {
        quote! {
            if !self.0.results.is_empty() {
                let context = self.0.context.upgrade();
                let result_val = context.get_value(self.0.results[0]);
                fmt.write(format!(" : {}", result_val.ty()))?;
            }
        }
    } else {
        quote! {}
    };

    let regions = if regions.len() == 1 && regions[0].single_block {
        make_single_block_region_printer(&regions[0])
    } else {
        quote! {}
    };

    quote! {
        fn print<'a, 'b: 'a>(&'a self, fmt: &'a mut tir::IRFormatter<'b>) -> Result<(), std::fmt::Error> {
            #result_prefix
            fmt.write(#op_name)?;
            #operand_printer
            // Print generic attribute dict if any
            if !self.attributes().is_empty() {
                fmt.write(" ")?;
                fmt.write("{")?;
                let mut first = true;
                for attr in self.attributes() {
                    if !first { fmt.write(", ")?; }
                    first = false;
                    fmt.write(&attr.name)?;
                    fmt.write(" = ")?;
                    attr.value.print(fmt)?;
                }
                fmt.write("}")?;
            }

            #result_suffix

            if self.regions().len() == 0 {
                fmt.write("\n")?;
            }

            #regions

            Ok(())
        }
    }
}

fn make_single_block_region_printer(region: &Region) -> proc_macro2::TokenStream {
    let name = format_ident!("{}", region.name);
    quote! {
        fmt.writeln(" {")?;
        let context = self.0.context.upgrade();
        fmt.push();
        for op in self.#name().iter(context.clone()) {
            let dyn_op = op.as_dyn_op();
            dyn_op.print(fmt)?;
        }
        fmt.pop();
        fmt.writeln("}")?;
    }
}

fn make_parser(
    builder_name: &Ident,
    regions: &[Region],
    operands: &[String],
    has_results: bool,
) -> proc_macro2::TokenStream {
    let (region_parsers, region_builders) = if regions.len() == 1 && regions[0].single_block {
        let region_name = format_ident!("{}", regions[0].name);
        (
            quote! {
               let #region_name = parser.parse_single_block_region(context)?.id();
            },
            quote! {
                .#region_name(#region_name)
            },
        )
    } else {
        (quote! {}, quote! {})
    };

    let operand_parsers: Vec<_> = operands
        .iter()
        .enumerate()
        .map(|(i, op_name)| {
            let field = format_ident!("{}", op_name);
            let comma = if i > 0 {
                quote! { parser.parse_token(","); }
            } else {
                quote! {}
            };
            quote! {
                #comma
                if let Some(ref_name) = parser.parse_value_ref() {
                    if let Ok(num) = ref_name.parse::<u32>() {
                        builder = builder.#field(tir::ValueId::from_number(num));
                    }
                }
            }
        })
        .collect();

    let result_parser = if has_results {
        quote! {
            if !parser.parse_token(":") {
                return Err((parser.span(), tir::Error::ExpectedToken(":")));
            }
            let result_ty = parser.parse_type()
                .ok_or_else(|| (parser.span(), tir::Error::ExpectedType))?;
            builder = builder.result_type(result_ty);
        }
    } else {
        quote! {}
    };

    quote! {
        fn parse<'src>(parser: &mut tir::parse::text::Parser<'src>, context: &tir::Context)
        -> Result<Box<dyn tir::Operation>, (tir::parse::Span, tir::Error)> {
           use tir::parse::common::Cursor;

           let mut parsed_attrs: Vec<tir::attributes::NamedAttribute> = vec![];

           let mut builder = #builder_name::new(context);

           #(#operand_parsers)*

           // Parse optional generic attribute dict: { key = value, ... }
           let mark = parser.pos();
           if parser.parse_token("{") {
               let mut ok = true;
               if !parser.parse_token("}") {
                   loop {
                       if let Some(name) = parser.parse_ident() {
                           if !parser.parse_token("=") { ok = false; break; }
                           let val = if let Some(s) = parser.parse_string() {
                               tir::attributes::AttributeValue::Str(s.to_string())
                           } else if parser.parse_token("%virt") {
                               if let Some(id) = parser.parse_number() {
                                   let mut class = None;
                                   if parser.parse_token(":") {
                                       if let Some(cls) = parser.parse_ident() { class = Some(cls.to_string()); } else { ok = false; }
                                   }
                                   tir::attributes::AttributeValue::Register(tir::attributes::RegisterAttr::Virtual { id: id as u32, class: class })
                               } else { ok = false; break; }
                           } else if let Some(n) = parser.parse_number() {
                               tir::attributes::AttributeValue::Int(n)
                           } else {
                               ok = false; break;
                           };
                           parsed_attrs.push(tir::attributes::NamedAttribute::new(name, val));
                           if parser.parse_token("}") { break; }
                           if !parser.parse_token(",") { ok = false; break; }
                       } else { ok = false; break; }
                   }
               }
               if !ok {
                   parser.set_pos(mark);
                   parsed_attrs.clear();
               }
           }

           #result_parser

           #region_parsers

            for a in parsed_attrs { builder = builder.attr(&a.name, a.value); }

            Ok(Box::new(builder
                #region_builders
                .build()))
        }
    }
}
