use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Attribute, FnArg, GenericArgument, ItemFn, Pat, PathArguments, ReturnType, Type,
    parse_macro_input,
};

#[proc_macro_attribute]
pub fn tool(_args: TokenStream, item: TokenStream) -> TokenStream {
    expand_tool(parse_macro_input!(item as ItemFn))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_tool(function: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    if function.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &function.sig.fn_token,
            "#[tool] requires an async function",
        ));
    }
    if !function.sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &function.sig.generics,
            "#[tool] does not support generic functions",
        ));
    }

    let name = &function.sig.ident;
    let python_name = syn::LitStr::new(&name.to_string(), proc_macro2::Span::call_site());
    let visibility = &function.vis;
    let description = docs(&function.attrs);
    let doc_attributes: Vec<_> = function
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("doc"))
        .collect();
    let output = result_output(&function.sig.output)?;
    let mut implementation = function.clone();
    for argument in &mut implementation.sig.inputs {
        if let FnArg::Typed(argument) = argument {
            argument.attrs.clear();
        }
    }
    let mut fields = Vec::new();
    let mut python_fields = Vec::new();
    let mut python_signature = Vec::new();
    let mut arguments = Vec::new();
    let mut saw_optional = false;

    for argument in &function.sig.inputs {
        let FnArg::Typed(argument) = argument else {
            return Err(syn::Error::new_spanned(
                argument,
                "#[tool] does not support methods",
            ));
        };
        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &argument.pat,
                "#[tool] parameters must be identifiers",
            ));
        };
        let ident = &pattern.ident;
        let ty = &argument.ty;
        let optional = is_option(ty);
        if saw_optional && !optional {
            return Err(syn::Error::new_spanned(
                argument,
                "required parameters must precede optional parameters",
            ));
        }
        saw_optional |= optional;
        let attrs = field_attributes(&argument.attrs)?;
        fields.push(quote!(#(#attrs)* #ident: #ty));
        python_fields.push(quote!(#ident: #ty));
        python_signature.push(if optional {
            quote!(#ident = None)
        } else {
            quote!(#ident)
        });
        arguments.push(ident);
    }

    Ok(quote! {
        #implementation

        #(#doc_attributes)*
        #visibility mod #name {
            use super::*;

            #[derive(
                ::e_agent_tool::__private::schemars::JsonSchema,
                ::e_agent_tool::__private::serde::Deserialize
            )]
            #[serde(deny_unknown_fields)]
            pub(crate) struct Input {
                #(#fields,)*
            }

            pub(crate) struct Definition;

            impl ::e_agent_tool::Tool for Definition {
                type Input = Input;
                type Output = #output;

                const NAME: &'static str = stringify!(#name);
                const DESCRIPTION: &'static str = #description;

                async fn call(input: Self::Input) -> ::e_agent_tool::Result<Self::Output> {
                    let Input { #(#arguments,)* } = input;
                    super::#name(#(#arguments),*).await
                }
            }

            #[::e_agent_tool::__private::pyo3::pyfunction(
                name = #python_name,
                signature = (#(#python_signature),*)
            )]
            pub(crate) fn python(
                py: ::e_agent_tool::__private::pyo3::Python,
                #(#python_fields,)*
            ) -> ::e_agent_tool::__private::pyo3::PyResult<
                ::e_agent_tool::__private::pyo3::Py<::e_agent_tool::__private::pyo3::PyAny>
            > {
                ::e_agent_tool::run::<Definition>(py, Input { #(#arguments,)* })
            }
        }
    })
}

fn is_option(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Path(path)
            if path.path.segments.last().is_some_and(|segment| segment.ident == "Option")
    )
}

fn field_attributes(attributes: &[Attribute]) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    attributes
        .iter()
        .map(|attribute| {
            if !attribute.path().is_ident("desc") {
                return Ok(quote!(#attribute));
            }

            let description = match &attribute.meta {
                syn::Meta::List(_) => attribute.parse_args::<syn::LitStr>()?,
                syn::Meta::NameValue(meta) => match &meta.value {
                    syn::Expr::Lit(expression) => match &expression.lit {
                        syn::Lit::Str(value) => value.clone(),
                        _ => {
                            return Err(syn::Error::new_spanned(
                                attribute,
                                "#[desc] requires a string",
                            ));
                        }
                    },
                    _ => {
                        return Err(syn::Error::new_spanned(
                            attribute,
                            "#[desc] requires a string",
                        ));
                    }
                },
                syn::Meta::Path(_) => {
                    return Err(syn::Error::new_spanned(
                        attribute,
                        "use #[desc(\"...\")] or #[desc = \"...\"]",
                    ));
                }
            };
            Ok(quote!(#[doc = #description]))
        })
        .collect()
}

fn result_output(output: &ReturnType) -> syn::Result<&Type> {
    let ReturnType::Type(_, ty) = output else {
        return Err(syn::Error::new_spanned(
            output,
            "#[tool] return type must be Result<T>",
        ));
    };
    let Type::Path(path) = ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            output,
            "#[tool] return type must be Result<T>",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err(syn::Error::new_spanned(output, "missing return type"));
    };
    if segment.ident != "Result" {
        return Err(syn::Error::new_spanned(
            output,
            "#[tool] return type must be Result<T>",
        ));
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            output,
            "#[tool] return type must be Result<T>",
        ));
    };
    arguments
        .args
        .iter()
        .find_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .ok_or_else(|| syn::Error::new_spanned(output, "Result<T> is missing T"))
}

fn docs(attributes: &[Attribute]) -> String {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("doc"))
        .filter_map(|attribute| attribute.meta.require_name_value().ok())
        .filter_map(|meta| match &meta.value {
            syn::Expr::Lit(expression) => match &expression.lit {
                syn::Lit::Str(value) => Some(value.value()),
                _ => None,
            },
            _ => None,
        })
        .map(|line| line.trim().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}
