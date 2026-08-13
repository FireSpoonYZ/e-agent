use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Attribute, FnArg, GenericArgument, Ident, Item, ItemFn, ItemMod, Pat, PathArguments,
    ReturnType, Type, parse_macro_input,
};

/// Describe a tool extension module and wire it to the host.
///
/// ```ignore
/// #[extension(description = "...", system_prompt = "...")]
/// mod todo { ... }
/// ```
#[proc_macro_attribute]
pub fn extension(args: TokenStream, item: TokenStream) -> TokenStream {
    let arguments = proc_macro2::TokenStream::from(args);
    expand_extension(arguments, parse_macro_input!(item as ItemMod))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Mark the single state struct of an `#[extension]` module.
///
/// `#[extension]` consumes this attribute, so reaching this expansion means the
/// struct is not inside an extension module.
#[proc_macro_attribute]
pub fn state(_args: TokenStream, item: TokenStream) -> TokenStream {
    let item = proc_macro2::TokenStream::from(item);
    let error = syn::Error::new_spanned(
        &item,
        "#[state] is only allowed on a struct inside an #[extension] module",
    )
    .into_compile_error();
    quote!(#item #error).into()
}

#[proc_macro_attribute]
pub fn tool(_args: TokenStream, item: TokenStream) -> TokenStream {
    expand_tool(parse_macro_input!(item as ItemFn), None)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct ExtensionArgs {
    description: syn::LitStr,
    system_prompt: syn::LitStr,
}

fn parse_extension_args(
    args: proc_macro2::TokenStream,
    span: proc_macro2::Span,
) -> syn::Result<ExtensionArgs> {
    let mut description = None;
    let mut system_prompt = None;
    let parsed =
        syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated
            .parse2(args)?;

    for meta in parsed {
        let value = match &meta.value {
            syn::Expr::Lit(expression) => match &expression.lit {
                syn::Lit::Str(value) => value.clone(),
                _ => {
                    return Err(syn::Error::new_spanned(
                        &meta.value,
                        "#[extension] values must be strings",
                    ));
                }
            },
            _ => {
                return Err(syn::Error::new_spanned(
                    &meta.value,
                    "#[extension] values must be strings",
                ));
            }
        };
        if meta.path.is_ident("description") {
            description = Some(value);
        } else if meta.path.is_ident("system_prompt") {
            system_prompt = Some(value);
        } else {
            return Err(syn::Error::new_spanned(
                &meta.path,
                "#[extension] accepts only description and system_prompt",
            ));
        }
    }

    let description = description
        .ok_or_else(|| syn::Error::new(span, "#[extension] requires description = \"...\""))?;
    if description.value().trim().is_empty() {
        return Err(syn::Error::new_spanned(
            &description,
            "#[extension] description must not be empty",
        ));
    }

    Ok(ExtensionArgs {
        description,
        system_prompt: system_prompt
            .unwrap_or_else(|| syn::LitStr::new("", proc_macro2::Span::call_site())),
    })
}

use syn::parse::Parser as _;

fn expand_extension(
    args: proc_macro2::TokenStream,
    mut module: ItemMod,
) -> syn::Result<proc_macro2::TokenStream> {
    let ExtensionArgs {
        description,
        system_prompt,
    } = parse_extension_args(args, module.ident.span())?;

    let Some((_, items)) = module.content.take() else {
        return Err(syn::Error::new_spanned(
            &module,
            "#[extension] requires an inline module body",
        ));
    };

    let mut state_struct: Option<Ident> = None;
    for item in &items {
        if let Item::Struct(structure) = item
            && structure
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("state"))
        {
            if let Some(existing) = &state_struct {
                return Err(syn::Error::new_spanned(
                    &structure.ident,
                    format!("#[extension] already has a #[state] struct `{existing}`"),
                ));
            }
            state_struct = Some(structure.ident.clone());
        }
    }

    let mut expanded = Vec::new();
    let mut tools = Vec::new();
    for item in items {
        match item {
            Item::Struct(mut structure) => {
                structure
                    .attrs
                    .retain(|attribute| !attribute.path().is_ident("state"));
                expanded.push(quote!(#structure));
            }
            Item::Fn(mut function) => {
                let is_tool = function
                    .attrs
                    .iter()
                    .any(|attribute| attribute.path().is_ident("tool"));
                if !is_tool {
                    expanded.push(quote!(#function));
                    continue;
                }
                function
                    .attrs
                    .retain(|attribute| !attribute.path().is_ident("tool"));
                tools.push(function.sig.ident.clone());
                expanded.push(expand_tool(function, state_struct.as_ref())?);
            }
            other => expanded.push(quote!(#other)),
        }
    }

    let state_items = state_struct.as_ref().map(|state| {
        quote! {
            #[doc(hidden)]
            pub(crate) static __E_AGENT_STATES: ::e_agent_tool::SessionStates<#state> =
                ::e_agent_tool::SessionStates::new();

            /// Extension state is shared across sessions and threads.
            const _: fn() = || {
                fn __e_agent_assert_state<T: ::std::default::Default + Send + Sync + 'static>() {}
                __e_agent_assert_state::<#state>();
            };
        }
    });
    let drop_state = if state_items.is_some() {
        quote!(__E_AGENT_STATES.drop_session(::e_agent_tool::SessionId(session));)
    } else {
        quote!(let _ = session;)
    };

    let name = &module.ident;
    let python_name = syn::LitStr::new(&name.to_string(), name.span());
    let visibility = &module.vis;
    let attributes = &module.attrs;

    Ok(quote! {
        #(#attributes)*
        #visibility mod #name {
            #(#expanded)*

            #state_items

            /// Set or clear the cancellation flag for this extension.
            ///
            /// A cdylib links its own copy of the tool runtime, so the host
            /// cannot reach these statics directly and drives them through
            /// Python instead.
            #[::e_agent_tool::__private::pyo3::pyfunction]
            #[pyo3(name = "_set_cancelled")]
            pub fn __e_agent_set_cancelled(cancelled: bool) {
                if cancelled {
                    ::e_agent_tool::cancel();
                } else {
                    ::e_agent_tool::reset();
                }
            }

            /// Bind the session whose state stateful tools should use.
            #[::e_agent_tool::__private::pyo3::pyfunction]
            #[pyo3(name = "__e_agent_set_session__")]
            pub fn __e_agent_set_session(session: u64) {
                ::e_agent_tool::set_current_session(::e_agent_tool::SessionId(session));
            }

            /// Unbind the current session.
            #[::e_agent_tool::__private::pyo3::pyfunction]
            #[pyo3(name = "__e_agent_clear_session__")]
            pub fn __e_agent_clear_session() {
                ::e_agent_tool::clear_current_session();
            }

            /// Drop one session's state, keeping every other session intact.
            #[::e_agent_tool::__private::pyo3::pyfunction]
            #[pyo3(name = "__e_agent_drop_session__")]
            pub fn __e_agent_drop_session(session: u64) {
                #drop_state
            }

            #[::e_agent_tool::__private::pyo3::pymodule(name = #python_name)]
            fn __e_agent_pymodule(
                module: &::e_agent_tool::__private::pyo3::Bound<
                    '_,
                    ::e_agent_tool::__private::pyo3::types::PyModule
                >,
            ) -> ::e_agent_tool::__private::pyo3::PyResult<()> {
                use ::e_agent_tool::__private::pyo3::types::PyModuleMethods as _;

                module.add_function(::e_agent_tool::__private::pyo3::wrap_pyfunction!(
                    __e_agent_set_cancelled, module)?)?;
                module.add_function(::e_agent_tool::__private::pyo3::wrap_pyfunction!(
                    __e_agent_set_session, module)?)?;
                module.add_function(::e_agent_tool::__private::pyo3::wrap_pyfunction!(
                    __e_agent_clear_session, module)?)?;
                module.add_function(::e_agent_tool::__private::pyo3::wrap_pyfunction!(
                    __e_agent_drop_session, module)?)?;
                #(module.add_function(::e_agent_tool::__private::pyo3::wrap_pyfunction!(
                    #tools::python, module)?)?;)*

                let functions = vec![#(::e_agent_tool::tool_function::<#tools::Definition>()
                    .map_err(|err| ::e_agent_tool::__private::pyo3::exceptions::PyValueError::new_err(
                        format!("{err:#}")))?),*];
                let extension = ::e_agent_tool::ToolExtension {
                    name: #python_name.to_string(),
                    description: #description.to_string(),
                    system_prompt: #system_prompt.to_string(),
                    functions,
                };
                let metadata = ::e_agent_tool::__private::serde_json::to_string(&extension)
                    .map_err(|err| ::e_agent_tool::__private::pyo3::exceptions::PyValueError::new_err(
                        err.to_string()))?;
                module.add("__e_agent_extension__", metadata)
            }
        }
    })
}

/// A `#[state]` tool parameter: its identifier and whether it is `&mut`.
struct StateParam {
    ident: Ident,
    mutable: bool,
}

fn state_param(
    argument: &syn::PatType,
    ident: &Ident,
    extension_state: Option<&Ident>,
) -> syn::Result<StateParam> {
    let Some(expected) = extension_state else {
        return Err(syn::Error::new_spanned(
            argument,
            "#[state] parameter requires a #[state] struct in the same #[extension] module",
        ));
    };
    let Type::Reference(reference) = argument.ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            &argument.ty,
            "#[state] parameter must be &State or &mut State",
        ));
    };
    let Type::Path(path) = reference.elem.as_ref() else {
        return Err(syn::Error::new_spanned(
            &argument.ty,
            "#[state] parameter must be &State or &mut State",
        ));
    };
    let matches = path
        .path
        .segments
        .last()
        .is_some_and(|segment| &segment.ident == expected && segment.arguments.is_none());
    if !matches {
        return Err(syn::Error::new_spanned(
            &argument.ty,
            format!("#[state] parameter type must be `{expected}`"),
        ));
    }

    Ok(StateParam {
        ident: ident.clone(),
        mutable: reference.mutability.is_some(),
    })
}

fn expand_tool(
    function: ItemFn,
    extension_state: Option<&Ident>,
) -> syn::Result<proc_macro2::TokenStream> {
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
    let mut input_fields = Vec::new();
    let mut call_arguments = Vec::new();
    let mut state = None::<StateParam>;
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

        if argument
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("state"))
        {
            if state.is_some() {
                return Err(syn::Error::new_spanned(
                    argument,
                    "#[tool] accepts at most one #[state] parameter",
                ));
            }
            state = Some(state_param(argument, ident, extension_state)?);
            let ident = ident.clone();
            call_arguments.push(quote!(#ident));
            continue;
        }

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
        python_fields.push(if optional {
            quote!(
                #ident: ::std::option::Option<
                    ::e_agent_tool::__private::pyo3::Bound<
                        '_,
                        ::e_agent_tool::__private::pyo3::PyAny
                    >
                >
            )
        } else {
            quote!(
                #ident: ::e_agent_tool::__private::pyo3::Bound<
                    '_,
                    ::e_agent_tool::__private::pyo3::PyAny
                >
            )
        });
        python_signature.push(if optional {
            quote!(#ident = None)
        } else {
            quote!(#ident)
        });
        input_fields.push(ident.clone());
        call_arguments.push(quote!(#ident));
    }

    let state_binding = state.map(|state| {
        let ident = state.ident;
        let borrow = if state.mutable {
            quote!(&mut *__e_agent_state)
        } else {
            quote!(&*__e_agent_state)
        };
        quote! {
            let mut __e_agent_state = super::__E_AGENT_STATES.current();
            let #ident = #borrow;
        }
    });

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
                    let Input { #(#input_fields,)* } = input;
                    #state_binding
                    super::#name(#(#call_arguments),*).await
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
                use ::e_agent_tool::__private::pyo3::types::PyDictMethods as _;

                let input = ::e_agent_tool::__private::pyo3::types::PyDict::new(py);
                #(input.set_item(stringify!(#input_fields), #input_fields)?;)*
                let input = ::e_agent_tool::input_from_python::<Input>(py, &input)?;
                ::e_agent_tool::run::<Definition>(py, input)
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
