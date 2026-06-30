mod dialect;
mod operation;
mod ty;
mod utils;

use proc_macro::TokenStream;

use dialect::construct_dialect;
use operation::construct_operation;
use ty::construct_tir_type;

#[proc_macro]
pub fn dialect(item: TokenStream) -> TokenStream {
    construct_dialect(item)
}

#[proc_macro]
pub fn operation(item: TokenStream) -> TokenStream {
    construct_operation(item)
}

#[proc_macro_derive(TirType, attributes(tir_type))]
pub fn derive_tir_type(item: TokenStream) -> TokenStream {
    construct_tir_type(item)
}
