mod dialect;
mod graph;
mod operation;
mod utils;

use proc_macro::TokenStream;

use dialect::construct_dialect;
use graph::construct_simple_node;
use operation::construct_operation;

#[proc_macro]
pub fn dialect(item: TokenStream) -> TokenStream {
    construct_dialect(item)
}

#[proc_macro]
pub fn operation(item: TokenStream) -> TokenStream {
    construct_operation(item)
}

#[proc_macro_derive(SimpleNode, attributes(simple_node, leaf, arity))]
pub fn derive_graph_node(item: TokenStream) -> TokenStream {
    construct_simple_node(item)
}
