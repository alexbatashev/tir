use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Expr, ExprStruct, Ident, Member, parse::Parse, parse_macro_input};

use crate::utils::{expr_as_string, field_name};

pub fn construct_operation(item: TokenStream) -> TokenStream {
    let Operation {
        struct_name,
        name,
        dialect,
        regions,
        attributes,
        roles,
    } = parse_macro_input!(item as Operation);

    let builder_name = format_ident!("{}Builder", struct_name.to_string());

    let printer = make_generic_printer(&dialect, &name, &[], &regions);

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

    let parser = make_parser(&builder_name, &regions);

    let verifier = make_attribute_verifier(&attributes);
    let roles_table = make_roles_table(&struct_name, &roles);

    quote! {
        pub struct #struct_name(std::sync::Arc<tir::OpInstance>);

        pub struct #builder_name {
            context: tir::Context,
            attributes: Vec<tir::attributes::NamedAttribute>,
            #(#region_fields,)*
        }

        impl #struct_name {
            #region_accessors
            #roles_table
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

            fn operands(&self) -> &[tir::Value] {
                todo!()
            }

            fn attributes(&self) -> &[tir::attributes::NamedAttribute] {
                &self.0.attributes
            }
        }

        impl #builder_name {
            pub fn new(context: &tir::Context) -> #builder_name {
                Self {
                    context: context.clone(),
                    attributes: vec![],
                    #(#region_defaults,)*
                }
            }

            #(#region_builders)*

            pub fn attr(mut self, name: &str, value: tir::attributes::AttributeValue) -> Self {
                self.attributes.push(tir::attributes::NamedAttribute::new(name, value));
                self
            }

            pub fn build(self) -> #struct_name {
                let mut regions = vec![];

                #(#region_fills)*

                #verifier

                let instance = tir::OpInstance {
                    id: tir::OpId::invalid(),
                    name: #name,
                    dialect: #dialect,
                    context: self.context.as_context_ref(),
                    operands: vec![],
                    results: vec![],
                    regions,
                    attributes: self.attributes,
                };

                let instance = self.context.add_operation(instance);

                #struct_name(instance)
            }
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

        Ok(Operation {
            struct_name,
            name,
            dialect,
            regions,
            attributes,
            roles,
        })
    }
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

fn make_generic_printer(
    dialect: &str,
    name: &str,
    _operands: &[()],
    regions: &[Region],
) -> proc_macro2::TokenStream {
    let op_name = if dialect == "builtin" {
        name
    } else {
        &format!("{}.{}", dialect, name)
    };

    let regions = if regions.len() == 1 && regions[0].single_block {
        make_single_block_region_printer(&regions[0])
    } else {
        quote! {}
    };

    quote! {
        fn print<'a, 'b: 'a>(&'a self, fmt: &'a mut tir::IRFormatter<'b>) -> Result<(), std::fmt::Error> {
            fmt.write(#op_name)?;
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

fn make_parser(builder_name: &Ident, regions: &[Region]) -> proc_macro2::TokenStream {
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

    quote! {
        fn parse<'src>(parser: &mut tir::parse::text::Parser<'src>, context: &tir::Context)
        -> Result<Box<dyn tir::Operation>, (tir::parse::Span, tir::Error)> {
           // Parse optional generic attribute dict: { key = value, ... }
           let mut parsed_attrs: Vec<tir::attributes::NamedAttribute> = vec![];
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

           #region_parsers

            let mut builder = #builder_name::new(context);
            for a in parsed_attrs { builder = builder.attr(&a.name, a.value); }

            Ok(Box::new(builder
                #region_builders
                .build()))
        }
    }
}
