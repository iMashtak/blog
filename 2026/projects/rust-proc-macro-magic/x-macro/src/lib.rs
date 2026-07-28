use proc_macro::TokenStream;

#[proc_macro]
pub fn select(body: TokenStream) -> TokenStream {
    x_macro_lib::make_select(body.into()).into()
}