use proc_macro::TokenStream;
use syn::ItemFn;
use quote::quote;

#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    let args: proc_macro2::TokenStream = args.into();
    let ItemFn { attrs, vis, sig, block } = syn::parse2(item.into()).unwrap();
    let stmts = &block.stmts;
    let res = quote! {
        #[cfg(not(feature = "current-thread"))]
        #[tokio::test(#args)]
        #(#attrs)* #vis #sig {
            #(#stmts)*
        }
        #[cfg(feature = "current-thread")]
        #[tokio::test(#args)]
        #(#attrs)* #vis #sig {
            tokio::task::LocalSet::new().run_until(async move {
                #(#stmts)*
            }).await
        }
    };
    // panic!("{}", res);
    res.into()
}

#[proc_macro_attribute]
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    let args: proc_macro2::TokenStream = args.into();
    let ItemFn { attrs, vis, sig, block } = syn::parse2(item.into()).unwrap();
    let stmts = &block.stmts;
    let res = quote! {
        #[cfg(not(feature = "current-thread"))]
        #[tokio::main(#args)]
        #(#attrs)* #vis #sig {
            #(#stmts)*
        }
        #[cfg(feature = "current-thread")]
        #[tokio::main(#args)]
        #(#attrs)* #vis #sig {
            tokio::task::LocalSet::new().run_until(async move {
                #(#stmts)*
            }).await
        }
    };
    // panic!("{}", res);
    res.into()
}
