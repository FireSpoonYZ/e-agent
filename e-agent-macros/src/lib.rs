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
            pub(crate) static __E_AGENT_STATES: ::e_agent_extension::SessionStates<#state> =
                ::e_agent_extension::SessionStates::new();

            /// Extension state is shared across sessions and threads.
            const _: fn() = || {
                fn __e_agent_assert_state<T: ::std::default::Default + Send + Sync + 'static>() {}
                __e_agent_assert_state::<#state>();
            };
        }
    });
    let drop_state = if state_items.is_some() {
        quote!(__E_AGENT_STATES.drop_session(::e_agent_extension::SessionId(session));)
    } else {
        quote!(let _ = session;)
    };

    let name = &module.ident;
    let extension_name = syn::LitStr::new(&name.to_string(), name.span());
    let visibility = &module.vis;
    let attributes = &module.attrs;

    Ok(quote! {
        #(#attributes)*
        #visibility mod #name {
            #(#expanded)*

            #state_items

            unsafe extern "C" fn __e_agent_metadata() -> ::e_agent_extension::AbiBuffer {
                let functions = vec![#(::e_agent_extension::tool_function::<#tools::Definition>()
                    .expect("generated tool metadata must serialize")),*];
                let extension = ::e_agent_extension::ToolExtension {
                    name: #extension_name.to_string(),
                    description: #description.to_string(),
                    system_prompt: #system_prompt.to_string(),
                    functions,
                };
                ::e_agent_extension::AbiBuffer::from_string(
                    ::e_agent_extension::__private::serde_json::to_string(&extension)
                        .expect("generated extension metadata must serialize")
                )
            }

            unsafe extern "C" fn __e_agent_start_call(
                session: u64,
                tool_ptr: *const u8,
                tool_len: usize,
                input_ptr: *const u8,
                input_len: usize,
                callback: ::e_agent_extension::CompletionCallback,
                user_data: *mut ::std::ffi::c_void,
            ) {
                if (tool_ptr.is_null() && tool_len != 0)
                    || (input_ptr.is_null() && input_len != 0)
                {
                    unsafe {
                        callback(
                            user_data,
                            ::e_agent_extension::AbiBuffer::from_string("invalid null input buffer".into()),
                            true,
                        );
                    }
                    return;
                }
                let tool_bytes = unsafe { ::std::slice::from_raw_parts(tool_ptr, tool_len) };
                let input = unsafe { ::std::slice::from_raw_parts(input_ptr, input_len) };
                let tool = match ::std::str::from_utf8(tool_bytes) {
                    Ok(tool) => tool,
                    Err(error) => {
                        unsafe {
                            callback(
                                user_data,
                                ::e_agent_extension::AbiBuffer::from_string(format!("invalid tool name: {error}")),
                                true,
                            );
                        }
                        return;
                    }
                };
                match tool {
                    #(stringify!(#tools) => unsafe {
                        ::e_agent_extension::start_tool_call::<#tools::Definition>(
                            session, input, callback, user_data
                        )
                    },)*
                    _ => unsafe {
                        callback(
                            user_data,
                            ::e_agent_extension::AbiBuffer::from_string(format!("unknown tool: {tool}")),
                            true,
                        );
                    },
                }
            }

            unsafe extern "C" fn __e_agent_drop_session(session: u64) {
                #drop_state
            }

            unsafe extern "C" fn __e_agent_set_cancelled(cancelled: bool) {
                if cancelled {
                    ::e_agent_extension::cancel();
                } else {
                    ::e_agent_extension::reset();
                }
            }

            #[cfg(not(test))]
            #[unsafe(no_mangle)]
            pub extern "C" fn e_agent_extension_v1() -> *const ::e_agent_extension::ExtensionV1 {
                static DESCRIPTOR: ::e_agent_extension::ExtensionV1 = ::e_agent_extension::ExtensionV1 {
                    abi_version: ::e_agent_extension::EXTENSION_ABI_VERSION,
                    metadata: __e_agent_metadata,
                    start_call: __e_agent_start_call,
                    drop_session: __e_agent_drop_session,
                    set_cancelled: __e_agent_set_cancelled,
                    free_buffer: ::e_agent_extension::free_buffer,
                };
                &DESCRIPTOR
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
                ::e_agent_extension::__private::schemars::JsonSchema,
                ::e_agent_extension::__private::serde::Deserialize
            )]
            #[serde(deny_unknown_fields)]
            pub(crate) struct Input {
                #(#fields,)*
            }

            pub(crate) struct Definition;

            impl ::e_agent_extension::Tool for Definition {
                type Input = Input;
                type Output = #output;

                const NAME: &'static str = stringify!(#name);
                const DESCRIPTION: &'static str = #description;

                async fn call(input: Self::Input) -> ::e_agent_extension::Result<Self::Output> {
                    let Input { #(#input_fields,)* } = input;
                    #state_binding
                    super::#name(#(#call_arguments),*).await
                }
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
