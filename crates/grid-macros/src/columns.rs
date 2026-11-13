//! What `#[derive(GridColumns)]` reads from an enum: the columns, their ids
//! and the crate path, with every check of the input.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::ToTokens as _;
use syn::{
    Attribute, Data, DeriveInput, Error, Fields, Ident, LitStr, Path, ext::IdentExt as _,
    parse_quote,
};

use crate::case::RenameRule;

/// The most columns a grid has: a `ColumnSet` is a `u128`.
pub(crate) const MAX_COLUMNS: usize = 128;

/// The enum attribute: `#[grid_columns(rename_all = "…", crate = "…")]`.
const ENUM_ATTRIBUTE: &str = "grid_columns";
/// The variant attribute: `#[grid_column(id = "…")]`.
const VARIANT_ATTRIBUTE: &str = "grid_column";

/// A column enum that passed every check.
pub(crate) struct ColumnEnum {
    /// The enum's name.
    pub(crate) name: Ident,
    /// The path of the grid crate: `::gpui_kit_grid` unless
    /// `#[grid_columns(crate = "…")]` names another.
    pub(crate) krate: Path,
    /// The columns in declaration order: the schema order.
    pub(crate) variants: Vec<ColumnVariant>,
}

/// One column of a [`ColumnEnum`].
pub(crate) struct ColumnVariant {
    pub(crate) ident: Ident,
    /// The column id: `#[grid_column(id = "…")]` or the variant name by the
    /// enum's rename rule.
    pub(crate) id: String,
    /// The variant's `#[cfg(…)]` attributes, repeated on everything the
    /// expansion writes for it.
    pub(crate) cfg_attrs: Vec<TokenStream>,
}

impl ColumnEnum {
    /// Reads and checks a derive input. Every error of the enum comes back
    /// at once, combined.
    pub(crate) fn parse(input: &DeriveInput) -> syn::Result<Self> {
        let data = match &input.data {
            Data::Enum(data) => data,
            Data::Struct(data) => return Err(not_an_enum(&data.struct_token)),
            Data::Union(data) => return Err(not_an_enum(&data.union_token)),
        };
        let mut errors = Errors::default();
        let options = EnumOptions::parse(&input.attrs, &mut errors);

        if !input.generics.params.is_empty() {
            errors.push(Error::new_spanned(
                &input.generics,
                "a column enum cannot have generic parameters",
            ));
        } else if let Some(where_clause) = &input.generics.where_clause {
            errors.push(Error::new_spanned(
                where_clause,
                "a column enum cannot have generic parameters",
            ));
        }
        if data.variants.is_empty() {
            errors.push(Error::new_spanned(
                &input.ident,
                "a column enum needs at least one variant",
            ));
        }

        let mut first_with_id: HashMap<String, &Ident> = HashMap::new();
        let mut variants = Vec::with_capacity(data.variants.len());
        for (ix, variant) in data.variants.iter().enumerate() {
            let ident = &variant.ident;
            if ix == MAX_COLUMNS {
                errors.push(Error::new_spanned(
                    ident,
                    format!(
                        "a grid has at most {MAX_COLUMNS} columns; `{ident}` is column {}",
                        ix + 1
                    ),
                ));
            }
            let shape = match &variant.fields {
                Fields::Unit => None,
                Fields::Named(_) => Some(" { .. }"),
                Fields::Unnamed(_) => Some("(..)"),
            };
            if let Some(shape) = shape {
                errors.push(Error::new_spanned(
                    &variant.fields,
                    format!("a column is a unit variant: `{ident}`, not `{ident}{shape}`"),
                ));
            }

            let explicit = parse_variant_id(&variant.attrs, &mut errors);
            let (id, id_span) = match &explicit {
                Some(literal) => (literal.value(), literal.span()),
                None => (
                    options.rename_all.apply(&ident.unraw().to_string()),
                    ident.span(),
                ),
            };
            if id.is_empty() {
                let message = match explicit {
                    Some(_) => "a column id cannot be empty",
                    None => "this name gives an empty column id; set `#[grid_column(id = \"…\")]`",
                };
                errors.push(Error::new(id_span, message));
            } else if let Some(first) = first_with_id.get(&id) {
                errors.push(Error::new(
                    id_span,
                    format!("the column id `{id}` is already used by `{first}`"),
                ));
            } else {
                first_with_id.insert(id.clone(), ident);
            }

            variants.push(ColumnVariant {
                ident: ident.clone(),
                id,
                cfg_attrs: variant
                    .attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("cfg"))
                    .map(|attr| attr.to_token_stream())
                    .collect(),
            });
        }

        errors.finish(Self {
            name: input.ident.clone(),
            krate: options
                .krate
                .unwrap_or_else(|| parse_quote!(::gpui_kit_grid)),
            variants,
        })
    }
}

fn not_an_enum(keyword: impl quote::ToTokens) -> Error {
    Error::new_spanned(
        keyword,
        "`GridColumns` can only be derived for an enum of columns",
    )
}

/// The keys of `#[grid_columns(…)]`.
#[derive(Default)]
struct EnumOptions {
    rename_all: RenameRule,
    krate: Option<Path>,
}

impl EnumOptions {
    fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut options = Self::default();
        let mut rename_all_set = false;
        for attr in attrs {
            if attr.path().is_ident(VARIANT_ATTRIBUTE) {
                errors.push(Error::new_spanned(
                    attr.path(),
                    "`grid_column` goes on a variant; the enum takes `grid_columns`",
                ));
                continue;
            }
            if !attr.path().is_ident(ENUM_ATTRIBUTE) {
                continue;
            }
            let parsed = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename_all") {
                    let literal: LitStr = meta.value()?.parse()?;
                    if rename_all_set {
                        errors.push(set_twice(&meta.path));
                        return Ok(());
                    }
                    rename_all_set = true;
                    match RenameRule::from_name(&literal.value()) {
                        Some(rule) => options.rename_all = rule,
                        None => errors.push(Error::new(
                            literal.span(),
                            format!(
                                "unknown `rename_all` rule `{:?}`; expected one of {}",
                                literal.value(),
                                RenameRule::ALL
                                    .iter()
                                    .map(|(name, _)| format!("{name:?}"))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            ),
                        )),
                    }
                    Ok(())
                } else if meta.path.is_ident("crate") {
                    let literal: LitStr = meta.value()?.parse()?;
                    if options.krate.is_some() {
                        errors.push(set_twice(&meta.path));
                        return Ok(());
                    }
                    match literal.parse::<Path>() {
                        Ok(path) => options.krate = Some(path),
                        Err(error) => errors.push(Error::new(
                            literal.span(),
                            format!("`crate` takes a path such as \"my_ui::grid\": {error}"),
                        )),
                    }
                    Ok(())
                } else {
                    Err(meta.error(format!(
                        "unknown `{ENUM_ATTRIBUTE}` key `{}`; expected `rename_all` or `crate`",
                        path_text(&meta.path),
                    )))
                }
            });
            if let Err(error) = parsed {
                errors.push(error);
            }
        }
        options
    }
}

/// The `id` of a variant's `#[grid_column(…)]`, if it has one.
fn parse_variant_id(attrs: &[Attribute], errors: &mut Errors) -> Option<LitStr> {
    let mut id = None;
    for attr in attrs {
        if attr.path().is_ident(ENUM_ATTRIBUTE) {
            errors.push(Error::new_spanned(
                attr.path(),
                "`grid_columns` goes on the enum; a variant takes `grid_column`",
            ));
            continue;
        }
        if !attr.path().is_ident(VARIANT_ATTRIBUTE) {
            continue;
        }
        let parsed = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("id") {
                let literal: LitStr = meta.value()?.parse()?;
                if id.is_some() {
                    errors.push(set_twice(&meta.path));
                } else {
                    id = Some(literal);
                }
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unknown `{VARIANT_ATTRIBUTE}` key `{}`; expected `id`",
                    path_text(&meta.path),
                )))
            }
        });
        if let Err(error) = parsed {
            errors.push(error);
        }
    }
    id
}

fn set_twice(key: &Path) -> Error {
    Error::new_spanned(key, format!("`{}` is set twice", path_text(key)))
}

/// A key as written: `id`, or `a::b`.
fn path_text(path: &Path) -> String {
    let segments = path
        .segments
        .iter()
        .map(|segment| segment.ident.unraw().to_string())
        .collect::<Vec<_>>()
        .join("::");
    match path.leading_colon {
        Some(_) => format!("::{segments}"),
        None => segments,
    }
}

/// The errors of one enum, combined into one `syn::Error`.
#[derive(Default)]
struct Errors(Option<Error>);

impl Errors {
    fn push(&mut self, error: Error) {
        match &mut self.0 {
            Some(first) => first.combine(error),
            None => self.0 = Some(error),
        }
    }

    fn finish<T>(self, value: T) -> syn::Result<T> {
        match self.0 {
            Some(error) => Err(error),
            None => Ok(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use quote::{ToTokens as _, format_ident};
    use syn::{DeriveInput, parse_quote};

    use super::{ColumnEnum, MAX_COLUMNS};

    fn parse(input: DeriveInput) -> ColumnEnum {
        match ColumnEnum::parse(&input) {
            Ok(columns) => columns,
            Err(error) => panic!("{error}"),
        }
    }

    /// The messages of every error of the enum, in order.
    fn errors(input: DeriveInput) -> Vec<String> {
        match ColumnEnum::parse(&input) {
            Ok(_) => panic!("the input passed"),
            Err(error) => error.into_iter().map(|error| error.to_string()).collect(),
        }
    }

    fn ids(columns: &ColumnEnum) -> Vec<&str> {
        columns
            .variants
            .iter()
            .map(|variant| variant.id.as_str())
            .collect()
    }

    #[test]
    fn test_parse_ids() {
        let columns = parse(parse_quote! {
            #[derive(Clone, GridColumns)]
            #[repr(u8)]
            enum TradeColumn {
                /// Documentation is ignored.
                Id = 10,
                BidSize = 3,
                #[grid_column(id = "px")]
                Price,
                HTTPStatus,
            }
        });
        assert_eq!(columns.name, "TradeColumn");
        assert_eq!(ids(&columns), ["id", "bid_size", "px", "http_status"]);
        let idents: Vec<String> = columns
            .variants
            .iter()
            .map(|variant| variant.ident.to_string())
            .collect();
        assert_eq!(idents, ["Id", "BidSize", "Price", "HTTPStatus"]);

        let columns = parse(parse_quote! {
            #[grid_columns(rename_all = "camelCase")]
            enum TradeColumn {
                Id,
                BidSize,
                #[grid_column(id = "Last price")]
                LastPrice,
            }
        });
        assert_eq!(ids(&columns), ["id", "bidSize", "Last price"]);
    }

    #[test]
    fn test_parse_errors() {
        let not_enum = "`GridColumns` can only be derived for an enum of columns";
        assert_eq!(
            errors(parse_quote!(
                struct Columns;
            )),
            [not_enum]
        );
        assert_eq!(errors(parse_quote!(union Columns { a: u8 })), [not_enum]);
        assert_eq!(
            errors(parse_quote!(
                enum Columns<T> {
                    A,
                }
            )),
            ["a column enum cannot have generic parameters"]
        );
        assert_eq!(
            errors(parse_quote!(
                enum Columns
                where
                    u8: Copy,
                {
                    A,
                }
            )),
            ["a column enum cannot have generic parameters"]
        );
        assert_eq!(
            errors(parse_quote!(
                enum Columns {
                    A,
                    Price(u8),
                    Size { lots: u8 },
                }
            )),
            [
                "a column is a unit variant: `Price`, not `Price(..)`",
                "a column is a unit variant: `Size`, not `Size { .. }`",
            ]
        );
        assert_eq!(
            errors(parse_quote!(
                enum Columns {}
            )),
            ["a column enum needs at least one variant"]
        );
        assert_eq!(
            errors(parse_quote! {
                enum Columns {
                    Price,
                    #[grid_column(id = "price")]
                    Px,
                    #[grid_column(id = "")]
                    Size,
                }
            }),
            [
                "the column id `price` is already used by `Price`",
                "a column id cannot be empty",
            ]
        );
        // A rule can make two names one id.
        assert_eq!(
            errors(parse_quote! {
                #[grid_columns(rename_all = "lowercase")]
                enum Columns { BidSize, Bidsize }
            }),
            ["the column id `bidsize` is already used by `BidSize`"]
        );
        assert_eq!(
            errors(parse_quote! {
                #[grid_columns(rename_all = "snake_case", rename_all = "kebab-case", crate = "a", crate = "b")]
                enum Columns {
                    #[grid_column(id = "a")]
                    #[grid_column(id = "b")]
                    A,
                }
            }),
            [
                "`rename_all` is set twice",
                "`crate` is set twice",
                "`id` is set twice"
            ]
        );
        assert_eq!(
            errors(parse_quote! {
                #[grid_columns(rename = "kebab-case")]
                enum Columns {
                    #[grid_column(name = "a")]
                    A,
                }
            }),
            [
                "unknown `grid_columns` key `rename`; expected `rename_all` or `crate`",
                "unknown `grid_column` key `name`; expected `id`",
            ]
        );
        assert_eq!(
            errors(parse_quote! {
                #[grid_columns(rename_all = "SCREAMING")]
                enum Columns { A }
            }),
            [
                r#"unknown `rename_all` rule `"SCREAMING"`; expected one of "snake_case", "kebab-case", "camelCase", "PascalCase", "lowercase""#
            ]
        );
        assert_eq!(
            errors(parse_quote! {
                #[grid_columns(crate = "my ui")]
                enum Columns { A }
            }),
            [r#"`crate` takes a path such as "my_ui::grid": unexpected token"#]
        );
        assert_eq!(
            errors(parse_quote! {
                #[grid_column(id = "a")]
                enum Columns {
                    #[grid_columns(rename_all = "kebab-case")]
                    A,
                }
            }),
            [
                "`grid_column` goes on a variant; the enum takes `grid_columns`",
                "`grid_columns` goes on the enum; a variant takes `grid_column`",
            ]
        );
        // Every error of the enum at once, in source order.
        assert_eq!(
            errors(parse_quote! {
                enum Columns<T> {
                    Price(T),
                    #[grid_column(id = "price")]
                    Px,
                }
            }),
            [
                "a column enum cannot have generic parameters",
                "a column is a unit variant: `Price`, not `Price(..)`",
                "the column id `price` is already used by `Price`",
            ]
        );
    }

    #[test]
    fn test_parse_crate_path() {
        let path = |columns: ColumnEnum| columns.krate.to_token_stream().to_string();
        assert_eq!(
            path(parse(parse_quote!(
                enum Columns {
                    A,
                }
            ))),
            ":: gpui_kit_grid"
        );
        assert_eq!(
            path(parse(parse_quote! {
                #[grid_columns(crate = "my_ui::grid")]
                enum Columns { A }
            })),
            "my_ui :: grid"
        );
    }

    #[test]
    fn test_parse_cfg_variant() {
        let columns = parse(parse_quote! {
            enum Columns {
                Id,
                #[cfg(feature = "venue")]
                #[deprecated]
                Venue,
            }
        });
        let cfg_attrs: Vec<Vec<String>> = columns
            .variants
            .iter()
            .map(|variant| variant.cfg_attrs.iter().map(ToString::to_string).collect())
            .collect();
        assert_eq!(
            cfg_attrs,
            [vec![], vec![r#"# [cfg (feature = "venue")]"#.to_owned()]]
        );
        // The expansion repeats it on the element of `all()` and on both arms.
        let expanded = crate::expand::expand(&columns).to_string();
        assert_eq!(expanded.matches("# [cfg (feature = \"venue\")]").count(), 3);
        assert_eq!(ids(&columns), ["id", "venue"]);
    }

    #[test]
    fn test_128_limit() {
        let wide = |count: usize| -> DeriveInput {
            let variants = (0..count).map(|ix| format_ident!("C{ix}"));
            parse_quote!(enum Wide { #(#variants),* })
        };
        assert_eq!(parse(wide(MAX_COLUMNS)).variants.len(), 128);
        // One error, on the 129th variant only.
        assert_eq!(
            errors(wide(MAX_COLUMNS + 2)),
            ["a grid has at most 128 columns; `C128` is column 129"]
        );
    }
}
