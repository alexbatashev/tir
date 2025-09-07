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

    quote! {
        pub struct #struct_name(std::sync::Arc<tir::OpInstance>);

        pub struct #builder_name {
            context: tir::Context,
            #(#region_fields,)*
        }

        impl #struct_name {
            #region_accessors
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
        }

        impl #builder_name {
            pub fn new(context: &tir::Context) -> #builder_name {
                Self {
                    context: context.clone(),
                    #(#region_defaults,)*
                }
            }

            #(#region_builders)*

            pub fn build(self) -> #struct_name {
                let mut regions = vec![];

                #(#region_fills)*

                let instance = tir::OpInstance {
                    id: tir::OpId::invalid(),
                    name: #name,
                    dialect: #dialect,
                    context: self.context.as_context_ref(),
                    operands: vec![],
                    results: vec![],
                    regions,
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

        Ok(Operation {
            struct_name,
            name,
            dialect,
            regions,
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
           #region_parsers

            Ok(Box::new(#builder_name::new(context)
                #region_builders
                .build()))
        }
    }
}
