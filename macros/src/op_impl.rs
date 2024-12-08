#![allow(clippy::redundant_pattern_matching)]
#![allow(clippy::manual_unwrap_or_default)]

use darling::ast::NestedMeta;
use darling::{FromDeriveInput, FromField, FromMeta};
use proc_macro::TokenStream;
use quote::{format_ident, quote, quote_spanned, ToTokens};
use syn::parse::{Parse, ParseStream, Parser};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{parse_macro_input, ItemStruct, Meta, MetaList, Path, Token, Type};

#[derive(Debug)]
pub struct OpAttrs {
    pub attrs: Vec<Attr>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Attr(pub syn::Ident, pub syn::Type);

fn path_is_option(path: &Path) -> bool {
    path.leading_colon.is_none()
        && path.segments.len() == 1
        && path.segments.iter().next().unwrap().ident == "Option"
}

fn type_is_option(ty: &syn::Type) -> bool {
    match ty {
        Type::Path(ty_path) => path_is_option(&ty_path.path),
        _ => false,
    }
}

impl Parse for Attr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attr_name = input.parse::<syn::Ident>()?;
        input.parse::<Token![:]>()?;
        let attr_ty = input.parse::<syn::Type>()?;

        Ok(Self(attr_name, attr_ty))
    }
}

impl FromMeta for OpAttrs {
    fn from_meta(item: &syn::Meta) -> darling::Result<Self> {
        if let syn::Meta::List(list) = item {
            let parser = Punctuated::<Attr, Token![,]>::parse_separated_nonempty;
            let tokens = list.tokens.clone();
            let attrs = parser
                .parse(tokens.into())?
                .iter()
                .cloned()
                .collect::<Vec<Attr>>();

            return Ok(OpAttrs { attrs });
        }
        // I genuinely have no idea what kind of error to put here
        panic!("expected syn::MetaList");
    }
}

#[derive(Debug, FromDeriveInput)]
#[darling(attributes(operation), supports(struct_named))]
pub struct OpReceiver {
    pub ident: syn::Ident,
    pub data: darling::ast::Data<(), OpFieldReceiver>,
    pub name: String,
    pub dialect: syn::Ident,
    #[darling(default)]
    pub known_attrs: Option<OpAttrs>,
}

#[derive(Debug, FromMeta)]
pub struct OpArgs {
    pub name: String,
    pub path: syn::Path,
    #[darling(default)]
    pub known_attrs: Option<OpAttrs>,
}

#[derive(Default, Debug, FromMeta)]
pub struct RegionAttrs {
    #[darling(default)]
    pub single_block: bool,
    #[darling(default)]
    pub no_args: bool,
}

fn parse_region_attrs(attr: &syn::Attribute) -> Option<RegionAttrs> {
    if !attr.path().is_ident("region") {
        return None;
    }

    if let syn::Meta::Path(_) = &attr.meta {
        Some(RegionAttrs::default())
    } else {
        RegionAttrs::from_meta(&attr.meta).ok()
    }
}

#[derive(Debug)]
pub enum OpFieldAttrs {
    Region(RegionAttrs),
    Operand,
    Return,
    None,
}

fn transform_field_attrs(attrs: Vec<syn::Attribute>) -> darling::Result<OpFieldAttrs> {
    for attr in attrs {
        if let Some(region) = parse_region_attrs(&attr) {
            return Ok(OpFieldAttrs::Region(region));
        }
        if attr.path().is_ident("ret_type") {
            return Ok(OpFieldAttrs::Return);
        }
        if attr.path().is_ident("operand") {
            return Ok(OpFieldAttrs::Operand);
        }
    }

    Ok(OpFieldAttrs::None)
}

#[derive(Debug, FromField)]
#[darling(forward_attrs(region, ret_type, operand))]
pub struct OpFieldReceiver {
    pub ident: Option<syn::Ident>,
    pub ty: syn::Type,
    #[darling(with = transform_field_attrs)]
    pub attrs: OpFieldAttrs,
}

pub fn build_attr_accessors(attrs: &[Attr]) -> proc_macro2::TokenStream {
    let mut attr_accessors = vec![];

    for attr in attrs {
        let getter_name = format_ident!("get_{}_attr", attr.0);
        let setter_name = format_ident!("set_{}_attr", attr.0);
        let attr_str = attr.0.to_string();

        if type_is_option(&attr.1) {
            attr_accessors.push(quote! {
                pub fn #getter_name(&self) -> Option<tir_core::Attr> {
                    self.r#impl.attrs.get(#attr_str).cloned()
                }

                pub fn #setter_name<T>(&mut self, value: Option<T>) where tir_core::Attr: From<T> {
                    match value {
                        Some(value) => {
                            let attr = tir_core::Attr::from(value);
                            self.r#impl.attrs.insert(#attr_str.to_string(), attr);
                        },
                        None => { self.r#impl.attrs.remove(#attr_str); },
                    };
                }
            });
        } else {
            attr_accessors.push(quote! {
                pub fn #getter_name(&self) -> tir_core::Attr {
                    self.r#impl.attrs.get(#attr_str).unwrap().clone()
                }

                pub fn #setter_name<T>(&mut self, value: T) where tir_core::Attr: From<T> {
                    let attr = tir_core::Attr::from(value);
                    self.r#impl.attrs.insert(#attr_str.to_string(), attr);
                }
            });
        }
    }

    quote! {
        #(#attr_accessors)*
    }
}

fn build_op_builder(args: &OpArgs, op: &ItemStruct) -> proc_macro2::TokenStream {
    let span = op.ident.span();
    let builder_name = format_ident!("{}Builder", op.ident);
    let name = op.ident;
    let op_name = args.name;

    let fields = op
        .fields
        .iter()
        .map(|f| OpFieldReceiver::from_field(f).unwrap())
        .collect::<Vec<_>>();

    let builder_fields = fields.iter().map(|f| {
        let span = f.ident.as_ref().unwrap().span();

        let name = f.ident.clone().unwrap();
        let ty = f.ty.clone();

        quote_spanned! {span=>
            #name: Option<#ty>
        }
    });

    let builder_setters = fields.iter().map(|f| {
        let span = f.ident.as_ref().unwrap().span();
        let name = f.ident.clone().unwrap();
        let ty = f.ty.clone();

        quote_spanned! {span =>
            pub fn #name(mut self, v: #ty) -> Self {
                self.#name = Some(v);
                self
            }
        }
    });

    let builder_creators = fields.iter().map(|f| {
        let span = f.ident.as_ref().unwrap().span();
        let name = f.ident.clone().unwrap();

        quote_spanned! {span=>
            #name: None
        }
    });

    quote_spanned! {span =>
        pub struct #builder_name {
            context: tir_core::ContextRef,
            #(#builder_fields),*
        }

        impl #builder_name {
            #(#builder_setters)*

            pub fn build(self) {
                let context = self.context;
                let dialect = context.get_dialect_by_name(DIALECT_NAME).expect("Did you forget to register the dialect?");
                let dialect_id = dialect.get_id();
                let operation_id = dialect.get_operation_id(#op_name).expect("Did you forget to register operation?");
                let mut attrs = std::collections::HashMap::new();
            }
        }

        impl #name {
            fn builder(context: &tir_core::ContextRef) -> #builder_name {
                #builder_name {
                    context: context.clone(),
                    #(#builder_creators),*
                }
            }
        }
    }
}

fn build_inner_struct(op: &ItemStruct) -> proc_macro2::TokenStream {
    let name = &op.ident;
    let span = op.span();

    let inner_name = format_ident!("{}Inner", name);

    let fields = op.fields.iter().map(|f| {
        let name = f.ident.as_ref().unwrap();
        let ty = &f.ty;
        let span = f.span();

        quote_spanned! {span =>
            #name: #ty,
        }
    });

    quote_spanned! {span=>
        #[derive(Clone)]
        pub struct #inner_name {
            #(#fields)*
            attrs: std::collections::HashMap<String, tir_core::Attr>,
            dialect_id: u32,
        }
    }
}

fn derive_op_trait(args: &OpArgs, op: &ItemStruct) -> proc_macro2::TokenStream {
    let name = &op.ident;
    let span = op.ident.span();
    let name_str = args.name.as_str();

    quote_spanned! {span =>
        impl tir_core::Op for #name {
            fn get_operation_name(&self) -> &'static str { #name_str }

            fn get_attrs(&self) -> &std::collections::HashMap<String, tir_core::Attr> { &self.inner.attrs }
            fn add_attrs(&mut self, attrs: &std::collections::HashMap<String, tir_core::Attr>) { todo!() }

            fn get_context(&self) -> tir_core::ContextRef { self.context.upgrade().unwrap() }

            fn get_parent_region(&self) -> Option<tir_core::RegionRef> { todo!() }
            fn set_parent_region(&mut self, region: tir_core::RegionWRef) { todo!() }

            fn get_return_type(&self) -> Option<tir_core::Type> { todo!() }
            fn get_return_value(&self) -> Option<tir_core::Value> { todo!() }

            fn set_alloc_id(&mut self, id: tir_core::AllocId) { todo!() }
            fn get_alloc_id(&self) -> tir_core::AllocId { todo!() }

            fn get_dialect_id(&self) -> u32 { self.inner.dialect_id }

            fn get_regions(&self) -> tir_core::OpRegionIter { todo!() }
            fn has_regions(&self) -> bool { todo!() }

            #[doc(hidden)]
            fn has_trait(&self, type_id: std::any::TypeId) -> bool { todo!() }
            #[doc(hidden)]
            fn get_meta(&self) -> &'static linkme::DistributedSlice<[fn() -> tir_core::utils::CastableMeta]> { todo!() }
        }
    }
}

fn derive_validate_trait(op: &ItemStruct) -> proc_macro2::TokenStream {
    let name = &op.ident;

    quote! {
        impl tir_core::Validate for #name {
            fn validate(&self) -> Result<(), tir_core::ValidateErr> {
                use tir_core::OpValidator;
                self.validate_op()?;

                // #(
                //     self.#region_names().validate()?;
                // )*

                Ok(())
            }
        }
    }
}

fn build_wrapper_struct(args: &OpArgs, op: &ItemStruct) -> proc_macro2::TokenStream {
    let span = op.span();

    let derive_list = op
        .attrs
        .iter()
        .find(|attr| {
            if let Meta::List(list) = &attr.meta {
                let name = format!("{}", list.path.to_token_stream());
                name == "derive"
            } else {
                false
            }
        })
        .map(|attr| attr.to_token_stream() as proc_macro2::TokenStream);

    let derive_list = derive_list.unwrap_or(quote! {});

    let name = &op.ident;
    let inner_name = format_ident!("{}Inner", name);
    let name_str = args.name.as_str();

    quote_spanned! {span=>
        #derive_list
        pub struct #name {
            context: tir_core::ContextWRef,
            id: tir_core::AllocId,
            inner: #inner_name
        }

        impl #name {
            pub fn get_operation_name() -> &'static str { #name_str }
        }
    }
}

pub fn build_operation(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(args.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(darling::Error::from(e).write_errors());
        }
    };

    let args = match OpArgs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

    let op_struct = parse_macro_input!(input as ItemStruct);

    let inner = build_inner_struct(&op_struct);
    let wrapper = build_wrapper_struct(&args, &op_struct);
    let op_trait = derive_op_trait(&args, &op_struct);
    let validate_trait = derive_validate_trait(&op_struct);
    let builder = build_op_builder(&args, &op_struct);

    let name = &op_struct.ident;

    let path_str = format!("{}", args.path.to_token_stream()).replace(" ", "");
    let test_name = format_ident!(
        "test_{}_module_path",
        args.name.to_lowercase().replace(".", "_")
    );

    quote! {
        #wrapper
        #inner
        #op_trait
        #validate_trait
        #builder

        impl tir_core::Printable for #name {
            fn print(&self, fmt: &mut dyn tir_core::IRFormatter) {
                fmt.indent();
                if DIALECT_NAME != tir_core::builtin::DIALECT_NAME {
                    fmt.write_direct(DIALECT_NAME);
                    fmt.write_direct(".");
                }

                fmt.write_direct(self.get_operation_name());
                fmt.write_direct(" ");

                self.print_assembly(fmt);
                fmt.write_direct("\n");
            }
        }

        impl tir_core::OpAssembly for #name {
            fn print_assembly(&self, fmt: &mut dyn tir_core::IRFormatter) { todo!() }
            fn parse_assembly(input: tir_core::IRStrStream<'_>) -> lpl::ParseResult<tir_core::IRStrStream<'_>, tir_core::OpRef> { todo!() }

        }

        #[cfg(test)]
        #[test]
        fn #test_name() {
            assert_eq!(#path_str, std::module_path!());
        }
    }
    .into()
}
