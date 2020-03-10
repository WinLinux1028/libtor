extern crate proc_macro;
extern crate proc_macro2;
extern crate quote;
extern crate syn;

use proc_macro2::{Ident, TokenStream};
use quote::{quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{braced, parenthesized, parse_macro_input, token, Data, DeriveInput, Fields, Token};

#[derive(Debug)]
struct ExpandToArg {
    keyword: Ident,
    equal_token: Token![=],
    name: syn::Lit,
}

impl Parse for ExpandToArg {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        Ok(ExpandToArg {
            keyword: input.parse()?,
            equal_token: input.parse()?,
            name: input.parse()?,
        })
    }
}

#[derive(Debug)]
struct TestStruct {
    keyword: Ident,
    eq_token: Token![=],
    args_group: Option<TokenStream>,
    arrow_token: Token![=>],
    expected: syn::LitStr,
}

impl Parse for TestStruct {
    fn parse(input: ParseStream) -> Result<Self, syn::Error> {
        let keyword: Ident = input.parse()?;
        if keyword != "test" {
            return Err(syn::Error::new(keyword.span(), "expected `test`"));
        }

        let eq_token = input.parse()?;

        let args_group: Option<TokenStream> = if input.peek(token::Brace) {
            let content;
            braced!(content in input);
            let content: TokenStream = content.parse()?;

            Some(quote! {
                { #content }
            })
        } else if input.peek(token::Paren) {
            let content;
            parenthesized!(content in input);
            let content: TokenStream = content.parse()?;

            Some(quote! {
                ( #content )
            })
        } else {
            None
        };

        let arrow_token = input.parse()?;
        let expected = input.parse()?;
        if let syn::Lit::Str(expected) = expected {
            Ok(TestStruct {
                keyword,
                eq_token,
                args_group,
                arrow_token,
                expected,
            })
        } else {
            Err(syn::Error::new(keyword.span(), "expected a string literal"))
        }
    }
}

#[proc_macro_derive(Expand, attributes(expand_to))]
pub fn derive_helper_attr(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let enum_name = &input.ident;

    let (match_body, test_funcs) = match input.data {
        Data::Enum(data) => {
            let mut stream = TokenStream::new();
            let mut test_stream = TokenStream::new();

            for variant in data.variants {
                let span = &variant.span();
                let name = &variant.ident;
                let mut name_string = name.to_string();
                let mut test_count = 0;
                let mut implemented_with = false;

                let mut fmt_attr = None;

                for attr in &variant.attrs {
                    if attr.path.get_ident().is_none()
                        || attr.path.get_ident().unwrap() != "expand_to"
                    {
                        continue;
                    }

                    if attr.parse_args::<syn::Lit>().is_ok() {
                        fmt_attr = Some(attr);
                    } else if let Ok(arg) = attr.parse_args::<ExpandToArg>() {
                        if arg.keyword == "rename" {
                            if let syn::Lit::Str(lit_str) = arg.name {
                                name_string = lit_str.value();
                            } else {
                                let tokens = quote_spanned! {*span=>
                                    #enum_name::#name{..} => compile_error!("`rename` must be followed by a string literal, eg #[expand_to(rename = \"example\")]"),
                                };
                                stream.extend(tokens);
                            }
                        } else if arg.keyword == "with" {
                            let tokens = if let syn::Lit::Str(lit_str) = arg.name {
                                let ident = Ident::new(&lit_str.value(), *span);

                                let matcher = match variant.fields {
                                    Fields::Unnamed(_) => quote! {
                                        #enum_name::#name(..)
                                    },
                                    Fields::Named(_) => quote! {
                                        #enum_name::#name{..}
                                    },
                                    Fields::Unit => quote! {
                                        #enum_name::#name()
                                    },
                                };

                                quote_spanned! {*span=>
                                    #matcher => format!("{}", #ident(self)),
                                }
                            } else {
                                quote_spanned! {*span=>
                                    #enum_name::#name{..} => compile_error!("`with` must be followed by a string literal, eg #[expand_to(with = \"my_custom_function\")]"),
                                }
                            };

                            stream.extend(tokens);
                            implemented_with = true;
                        }
                    } else {
                        // TODO: add those example as doc attributes
                        if let Ok(parsed) = attr.parse_args::<TestStruct>() {
                            let test_name =
                                Ident::new(&format!("TEST_{}_{}", name, test_count), *span);
                            let args_group = &parsed.args_group.unwrap_or_default();
                            let expected = &parsed.expected;

                            let tokens = quote_spanned! {*span=>
                                #[test]
                                fn #test_name() {
                                    use Expand;

                                    let v = #enum_name::#name#args_group;
                                    println!("{:?} => {}", v, v.expand());
                                    assert_eq!(v.expand(), #expected);
                                }
                            };

                            test_stream.extend(tokens);
                            test_count += 1;
                        }
                    }
                }

                if implemented_with {
                    continue;
                }

                let ignore_filter = |field: &&syn::Field| {
                    !field.attrs.iter().any(|a| {
                        a.parse_args::<syn::Ident>()
                            .and_then(|ident| Ok(ident == "ignore"))
                            .unwrap_or(false)
                    })
                };
                let tokens = match (variant.fields, fmt_attr) {
                    (Fields::Named(_), None) => {
                        quote_spanned! {*span=>
                            #enum_name::#name{..} => compile_error!("Named fields require an explicit expansion attribute"),
                        }
                    }
                    (Fields::Named(fields), Some(attr)) => {
                        let args: TokenStream = attr.parse_args().unwrap();

                        let fmt_params = fields.named.iter().filter(ignore_filter).map(|f| {
                            let ident = &f.ident;
                            quote_spanned! {f.span()=> #ident = #ident }
                        });
                        let expand_params = fields.named.iter().map(|f| {
                            let ident = &f.ident;
                            quote_spanned! {f.span()=> #ident }
                        });

                        quote_spanned! {attr.span()=>
                            #enum_name::#name{#(#expand_params,)*} => format!(#args, #(#fmt_params, )*),
                        }
                    }
                    (Fields::Unnamed(fields), attr) => {
                        let expand_params = (0..fields.unnamed.len())
                            .map(|i| Ident::new(&format!("p_{}", i), i.span()));
                        let fmt_params = (0..fields.unnamed.len())
                            .filter(|i| ignore_filter(&&fields.unnamed[*i]))
                            .map(|i| Ident::new(&format!("p_{}", i), i.span()));

                        if let Some(attr) = attr {
                            let args: TokenStream = attr.parse_args().unwrap();
                            quote_spanned! {*span=>
                                #enum_name::#name(#(#expand_params, )*) => format!(#args, #(#fmt_params, )*),
                            }
                        } else {
                            let fmt_str = (0..fields.unnamed.len())
                                .map(|_| "{}")
                                .collect::<Vec<&str>>()
                                .join(" ");
                            let fmt_str = format!("{{}} \"{}\"", fmt_str); // {cmdName} + Wrap all the params between quotes
                            quote_spanned! {*span=>
                                #enum_name::#name(#(#expand_params, )*) => format!(#fmt_str, #name_string, #(#fmt_params, )*),
                            }
                        }
                    }
                    (Fields::Unit, None) => quote! {
                        #enum_name::#name => #name_string.to_string(),
                    },
                    (Fields::Unit, Some(attr)) => {
                        let args: TokenStream = attr.parse_args().unwrap();
                        quote! {
                            #enum_name::#name => #args.to_string(),
                        }
                    }
                };

                stream.extend(tokens);
            }

            (stream, test_stream)
        }
        _ => unimplemented!(),
    };

    let test_mod_name = Ident::new(
        &format!("_GENERATED_TESTS_FOR_{}", enum_name),
        enum_name.span(),
    );

    let name = input.ident;
    let expanded = quote! {
        impl Expand for #name {
            fn expand(&self) -> String {
                #[allow(unused)]
                #[allow(clippy::useless_format)]
                match self {
                    #match_body
                }
            }
        }

        #[cfg(test)]
        #[allow(non_snake_case)]
        mod #test_mod_name {
            use super::*;

            #test_funcs
        }
    };

    proc_macro::TokenStream::from(expanded)
}
