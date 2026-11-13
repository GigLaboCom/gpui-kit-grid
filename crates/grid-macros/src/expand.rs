//! The `GridColumns` implementation of a checked column enum.

use proc_macro2::{Literal, TokenStream};
use quote::quote;

use crate::columns::{ColumnEnum, ColumnVariant};

/// Writes the implementation a hand-written one would be: `all` lists the
/// variants in declaration order, `id` and `ix` match on them. `ix` counts
/// declaration order rather than casting (`self as u16`), so explicit
/// discriminants do not shift it.
pub(crate) fn expand(columns: &ColumnEnum) -> TokenStream {
    let ColumnEnum {
        name,
        krate,
        variants,
    } = columns;
    let mut all = TokenStream::new();
    let mut id_arms = TokenStream::new();
    let mut ix_arms = TokenStream::new();
    for (ix, variant) in variants.iter().enumerate() {
        let ColumnVariant {
            ident,
            id,
            cfg_attrs,
        } = variant;
        let ix = Literal::u16_suffixed(u16::try_from(ix).expect("at most 128 columns"));
        all.extend(quote!(#(#cfg_attrs)* Self::#ident,));
        id_arms.extend(quote!(#(#cfg_attrs)* Self::#ident => #id,));
        ix_arms.extend(quote!(#(#cfg_attrs)* Self::#ident => #krate::ColumnIx::new(#ix),));
    }
    quote! {
        #[automatically_derived]
        #[allow(deprecated)]
        impl #krate::GridColumns for #name {
            fn all() -> &'static [Self] {
                &[#all]
            }

            fn id(self) -> &'static str {
                match self {
                    #id_arms
                }
            }

            fn ix(self) -> #krate::ColumnIx {
                match self {
                    #ix_arms
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use syn::{DeriveInput, Expr, ImplItem, ItemImpl, Lit, parse_quote};

    use super::expand;
    use crate::columns::ColumnEnum;

    #[test]
    fn test_expand_shape() {
        let input: DeriveInput = parse_quote! {
            enum TradeColumn {
                Id,
                #[deprecated]
                BidSize = 7,
                #[grid_column(id = "px")]
                Price,
            }
        };
        let tokens = expand(&ColumnEnum::parse(&input).unwrap());
        let item: ItemImpl = syn::parse2(tokens).unwrap();

        let attrs: Vec<String> = item
            .attrs
            .iter()
            .map(|attr| quote::ToTokens::to_token_stream(attr).to_string())
            .collect();
        assert_eq!(
            attrs,
            ["# [automatically_derived]", "# [allow (deprecated)]"]
        );
        let (trait_path, _) = item.trait_.as_ref().unwrap();
        assert_eq!(
            quote::ToTokens::to_token_stream(trait_path).to_string(),
            ":: gpui_kit_grid :: GridColumns"
        );

        let methods: Vec<_> = item
            .items
            .iter()
            .map(|item| match item {
                ImplItem::Fn(method) => method,
                _ => panic!("only methods"),
            })
            .collect();
        let names: Vec<String> = methods
            .iter()
            .map(|method| method.sig.ident.to_string())
            .collect();
        assert_eq!(names, ["all", "id", "ix"]);

        let arms = |method: usize| match methods[method].block.stmts.as_slice() {
            [syn::Stmt::Expr(Expr::Match(expr), None)] => &expr.arms,
            _ => panic!("a match"),
        };
        // `id`: the ids as string literals.
        let ids: Vec<String> = arms(1)
            .iter()
            .map(|arm| match &*arm.body {
                Expr::Lit(expr) => match &expr.lit {
                    Lit::Str(id) => id.value(),
                    _ => panic!("a string"),
                },
                _ => panic!("a literal"),
            })
            .collect();
        assert_eq!(ids, ["id", "bid_size", "px"]);
        // `ix`: `0u16..n` in declaration order, whatever the discriminants.
        let ixs: Vec<String> = arms(2)
            .iter()
            .map(|arm| match &*arm.body {
                Expr::Call(call) => quote::ToTokens::to_token_stream(&call.args).to_string(),
                _ => panic!("a call"),
            })
            .collect();
        assert_eq!(ixs, ["0u16", "1u16", "2u16"]);
    }
}
