use proc_macro::TokenStream;

use quote::{format_ident, quote, ToTokens};
use syn::{
    parse::Parser, parse_macro_input, punctuated::Punctuated, Attribute, DeriveInput, Error, FnArg,
    ItemFn, ItemImpl, ItemTrait, LitStr, Meta, Pat, ReturnType, Token, TraitItem, Type,
};

#[proc_macro_derive(VoxExport, attributes(vox))]
pub fn derive_vox_export(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_vox_export(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn vox_trait(args: TokenStream, item: TokenStream) -> TokenStream {
    let mut item = parse_macro_input!(item as ItemTrait);
    match expand_vox_trait(parse_attr_args(args), &mut item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn vox_fn(args: TokenStream, item: TokenStream) -> TokenStream {
    let mut item = parse_macro_input!(item as ItemFn);
    match expand_vox_fn(parse_attr_args(args), &mut item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn vox_trait_impl(args: TokenStream, item: TokenStream) -> TokenStream {
    let _args = parse_attr_args(args);
    let item = parse_macro_input!(item as ItemImpl);
    match expand_vox_trait_impl(item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand_vox_export(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let ident = &input.ident;
    let vox_name = exported_name_from_attrs(&input.attrs)?.unwrap_or_else(|| ident.to_string());
    let trait_impls = trait_impls_from_attrs(&input.attrs, &vox_name)?;
    let doc = collect_vox_doc_string(&vox_name, &input.attrs, &input.attrs)?;
    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(fields) => fields.named.iter().collect::<Vec<_>>(),
            other => {
                return Err(Error::new_spanned(
                    other,
                    "VoxExport only supports structs with named fields",
                ));
            }
        },
        _ => {
            return Err(Error::new_spanned(
                input.ident,
                "VoxExport can only be derived for structs",
            ));
        }
    };
    let decode_fields = fields.iter().map(|field| {
        let field_ident = field.ident.as_ref().expect("named fields should have identifiers");
        let field_name = field_ident.to_string();
        let field_ty = &field.ty;
        quote! {
            #field_ident: <#field_ty as ::vox_runtime::host_exports::FromVoxFieldData>::from_vox_field_data(
                fields.remove(#field_name).ok_or_else(|| {
                    format!("missing field `{}` in `{}`", #field_name, #vox_name)
                })?,
            )?
        }
    });
    let encode_fields = fields.iter().map(|field| {
        let field_ident = field.ident.as_ref().expect("named fields should have identifiers");
        let field_name = field_ident.to_string();
        quote! {
            fields.insert(
                #field_name.to_owned(),
                ::vox_runtime::host_exports::IntoVoxFieldData::into_vox_field_data(self.#field_ident)?,
            );
        }
    });
    let exported_fields = fields.iter().map(|field| {
        let field_ident = field
            .ident
            .as_ref()
            .expect("named fields should have identifiers");
        let field_name = field_ident.to_string();
        let field_ty = &field.ty;
        let field_ty_text = field_ty.to_token_stream().to_string();
        quote! {
            ::voxlib_sdk::external_export::ExportedSurfaceField {
                name: #field_name,
                rust_type: #field_ty_text,
            }
        }
    });
    Ok(quote! {
        impl ::vox_runtime::host_exports::VoxHandleValue for #ident {
            fn vox_type_name() -> &'static str {
                #vox_name
            }

            fn from_vox_handle_data(
                data: ::vox_core::value::HandleData,
            ) -> ::core::result::Result<Self, ::std::string::String> {
                let ::vox_core::value::HandleData::Record(mut fields) = data else {
                    return ::core::result::Result::Err(format!(
                        "expected record handle data for `{}`",
                        #vox_name
                    ));
                };
                ::core::result::Result::Ok(Self {
                    #(#decode_fields),*
                })
            }

            fn into_vox_handle_data(
                self,
            ) -> ::core::result::Result<::vox_core::value::HandleData, ::std::string::String> {
                let mut fields = ::std::collections::BTreeMap::new();
                #(#encode_fields)*
                ::core::result::Result::Ok(::vox_core::value::HandleData::Record(fields))
            }
        }

        ::voxlib_sdk::external_export::inventory::submit! {
            ::voxlib_sdk::external_export::ExportedSurfaceRegistration {
                rust_name: stringify!(#ident),
                vox_name: #vox_name,
                kind: ::voxlib_sdk::external_export::ExportedSurfaceKind::Struct,
                fields: &[#(#exported_fields),*],
                order: line!(),
            }
        }

        #doc

        #(#trait_impls)*
    })
}

fn expand_vox_trait(
    args: syn::Result<Vec<Meta>>,
    item: &mut ItemTrait,
) -> syn::Result<proc_macro2::TokenStream> {
    let args = args?;
    let vox_name = exported_name_from_meta(&args)?.unwrap_or_else(|| item.ident.to_string());
    let method_exports = exported_trait_method_exports(&vox_name, item)?;
    strip_nested_vox_attrs_from_trait(item);
    let ident = &item.ident;

    let mut doc = extract_rust_doc_comments(&item.attrs);
    if let Some(d) = vox_doc_from_meta(&args) {
        doc = d;
    }
    let doc_submission: proc_macro2::TokenStream = if doc.is_empty() {
        proc_macro2::TokenStream::new()
    } else {
        quote! {
            ::voxlib_sdk::external_export::inventory::submit! {
                ::voxlib_sdk::external_export::ExportedDocstringRegistration {
                    vox_name: #vox_name,
                    doc: #doc,
                    order: line!(),
                }
            }
        }
    };

    Ok(quote! {
        #item

        ::voxlib_sdk::external_export::inventory::submit! {
            ::voxlib_sdk::external_export::ExportedSurfaceRegistration {
                rust_name: stringify!(#ident),
                vox_name: #vox_name,
                kind: ::voxlib_sdk::external_export::ExportedSurfaceKind::Trait,
                fields: &[],
                order: line!(),
            }
        }

        #(#method_exports)*

        #doc_submission
    })
}

fn expand_vox_fn(
    args: syn::Result<Vec<Meta>>,
    item: &mut ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let options = FunctionOptions::from_meta(&args?)?;
    let ident = &item.sig.ident;
    let vox_name = options.name.clone().unwrap_or_else(|| ident.to_string());
    let purity = options.purity.tokens();
    let return_type = match &item.sig.output {
        ReturnType::Default => "()".to_owned(),
        ReturnType::Type(_, ty) => ty.to_token_stream().to_string(),
    };
    let return_type_tokens = match &item.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => quote!(#ty),
    };
    let return_type_override = options.return_type_override.as_ref();

    let mut doc = extract_rust_doc_comments(&item.attrs);
    if let Some(d) = &options.doc {
        doc = d.clone();
    }
    let doc_submission: proc_macro2::TokenStream = if doc.is_empty() {
        proc_macro2::TokenStream::new()
    } else {
        quote! {
            ::voxlib_sdk::external_export::inventory::submit! {
                ::voxlib_sdk::external_export::ExportedDocstringRegistration {
                    vox_name: #vox_name,
                    doc: #doc,
                    order: line!(),
                }
            }
        }
    };

    let mut parameters = Vec::new();
    let mut invoker_bindings = Vec::new();
    let mut invoker_arguments = Vec::new();
    for input in &mut item.sig.inputs {
        let FnArg::Typed(parameter) = input else {
            return Err(Error::new_spanned(
                input,
                "vox_fn only supports free functions",
            ));
        };
        let name = match &*parameter.pat {
            Pat::Ident(ident) => ident.ident.to_string(),
            other => {
                return Err(Error::new_spanned(
                    other,
                    "vox_fn parameters must use simple identifiers",
                ));
            }
        };
        let ident = match &*parameter.pat {
            Pat::Ident(ident) => ident.ident.clone(),
            _ => unreachable!("simple identifier pattern already required"),
        };
        let has_default = take_default_marker(&mut parameter.attrs)?;
        let ty = (*parameter.ty).clone();
        let ty_text = ty.to_token_stream().to_string();
        parameters.push(quote! {
            ::voxlib_sdk::external_export::ExportedFunctionParameter {
                name: #name,
                rust_type: #ty_text,
                has_default: #has_default,
            }
        });
        let position = invoker_arguments.len();
        invoker_bindings.push(quote! {
            let #ident = <#ty as ::vox_runtime::host_exports::FromHostValue>::from_host_value(
                runtime,
                ::vox_runtime::host_exports::required_argument(arguments, #position, #name)?,
            )?;
        });
        invoker_arguments.push(quote!(#ident));
    }

    let return_override_tokens = match return_type_override {
        Some(value) => quote!(::core::option::Option::Some(#value)),
        None => quote!(::core::option::Option::None),
    };
    let invoke_ident = format_ident!("__vox_invoke_{}", ident);

    Ok(quote! {
        #item

        fn #invoke_ident(
            runtime: &mut ::vox_runtime::Runtime,
            arguments: &[::vox_runtime::HostCallArgument],
        ) -> ::core::result::Result<::vox_core::value::RuntimeValue, ::std::string::String> {
            #(#invoker_bindings)*
            let result: #return_type_tokens = #ident(#(#invoker_arguments),*);
            <#return_type_tokens as ::vox_runtime::host_exports::IntoHostValue>::into_host_value(
                result,
                runtime,
            )
        }

        ::voxlib_sdk::external_export::inventory::submit! {
            ::voxlib_sdk::external_export::ExportedFunctionRegistration {
                rust_name: stringify!(#ident),
                vox_name: #vox_name,
                purity: #purity,
                parameters: &[#(#parameters),*],
                return_rust_type: #return_type,
                return_type_override: #return_override_tokens,
                order: line!(),
            }
        }

        ::vox_runtime::host_exports::inventory::submit! {
            ::vox_runtime::host_exports::RegisteredHostFunctionImplementation {
                rust_name: stringify!(#ident),
                vox_name: #vox_name,
                invoke: #invoke_ident,
                order: line!(),
            }
        }

        #doc_submission
    })
}

fn expand_vox_trait_impl(item: ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    let Some((_, trait_path, _)) = &item.trait_ else {
        return Err(Error::new_spanned(
            item.impl_token,
            "#[vox_trait_impl] requires `impl Trait for Type` syntax",
        ));
    };

    let trait_name = trait_path
        .segments
        .last()
        .map(|seg| seg.ident.to_string())
        .ok_or_else(|| Error::new_spanned(trait_path, "could not extract trait name"))?;

    let Type::Path(type_path) = &*item.self_ty else {
        return Err(Error::new_spanned(
            &item.self_ty,
            "#[vox_trait_impl] requires a named struct type",
        ));
    };

    let struct_name = type_path
        .path
        .segments
        .last()
        .map(|seg| seg.ident.to_string())
        .ok_or_else(|| Error::new_spanned(type_path, "could not extract struct name"))?;

    Ok(quote! {
        #item

        ::voxlib_sdk::external_export::inventory::submit! {
            ::voxlib_sdk::external_export::ExportedTraitImplRegistration {
                struct_vox_name: #struct_name,
                trait_vox_name: #trait_name,
                order: line!(),
            }
        }
    })
}

fn parse_attr_args(args: TokenStream) -> syn::Result<Vec<Meta>> {
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    parser
        .parse(args)
        .map(|items| items.into_iter().collect::<Vec<_>>())
}

fn collect_vox_doc_string(
    vox_name: &str,
    attrs: &[Attribute],
    vox_attrs: &[Attribute],
) -> syn::Result<proc_macro2::TokenStream> {
    let mut doc = extract_rust_doc_comments(attrs);
    if let Some(vox_doc) = vox_doc_override(vox_attrs)? {
        doc = vox_doc;
    }
    if doc.is_empty() {
        return Ok(proc_macro2::TokenStream::new());
    }
    Ok(quote! {
        ::voxlib_sdk::external_export::inventory::submit! {
            ::voxlib_sdk::external_export::ExportedDocstringRegistration {
                vox_name: #vox_name,
                doc: #doc,
                order: line!(),
            }
        }
    })
}

fn extract_rust_doc_comments(attrs: &[Attribute]) -> String {
    let mut doc = String::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(expr) = &nv.value {
                if let syn::Lit::Str(s) = &expr.lit {
                    if !doc.is_empty() {
                        doc.push('\n');
                    }
                    doc.push_str(&s.value());
                }
            }
        }
    }
    doc
}

fn vox_doc_override(vox_attrs: &[Attribute]) -> syn::Result<Option<String>> {
    for attr in vox_attrs {
        if !attr.path().is_ident("vox") {
            continue;
        }
        for entry in parse_nested_meta(attr)? {
            let Meta::NameValue(value) = entry else {
                continue;
            };
            if value.path.is_ident("doc") {
                return Ok(Some(expect_lit_str(&value.value, "doc")?.value()));
            }
        }
    }
    Ok(None)
}

fn vox_doc_from_meta(meta: &[Meta]) -> Option<String> {
    for entry in meta {
        let Meta::NameValue(value) = entry else {
            continue;
        };
        if value.path.is_ident("doc") {
            if let syn::Expr::Lit(expr) = &value.value {
                if let syn::Lit::Str(s) = &expr.lit {
                    return Some(s.value());
                }
            }
        }
    }
    None
}

fn exported_name_from_attrs(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    for attr in attrs {
        if attr.path().is_ident("vox") {
            return exported_name_from_meta(&parse_nested_meta(attr)?);
        }
    }
    Ok(None)
}

fn trait_impls_from_attrs(
    attrs: &[Attribute],
    vox_name: &str,
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    let mut submissions = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("vox") {
            for entry in parse_nested_meta(attr)? {
                let Meta::NameValue(value) = entry else {
                    continue;
                };
                if value.path.is_ident("trait_impl") {
                    let trait_name = expect_lit_str(&value.value, "trait_impl")?.value();
                    submissions.push(quote! {
                        ::voxlib_sdk::external_export::inventory::submit! {
                            ::voxlib_sdk::external_export::ExportedTraitImplRegistration {
                                struct_vox_name: #vox_name,
                                trait_vox_name: #trait_name,
                                order: line!(),
                            }
                        }
                    });
                }
            }
        }
    }
    Ok(submissions)
}

fn exported_name_from_meta(meta: &[Meta]) -> syn::Result<Option<String>> {
    let mut name = None;
    for entry in meta {
        let Meta::NameValue(value) = entry else {
            continue;
        };
        if value.path.is_ident("name") {
            match &value.value {
                syn::Expr::Lit(expr) => match &expr.lit {
                    syn::Lit::Str(value) => name = Some(value.value()),
                    other => {
                        return Err(Error::new_spanned(other, "expected string literal"));
                    }
                },
                other => return Err(Error::new_spanned(other, "expected string literal")),
            }
        }
    }
    Ok(name)
}

fn parse_nested_meta(attr: &Attribute) -> syn::Result<Vec<Meta>> {
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    parser
        .parse2(attr.meta.require_list()?.tokens.clone())
        .map(|items| items.into_iter().collect::<Vec<_>>())
}

fn exported_trait_method_exports(
    trait_vox_name: &str,
    item: &mut ItemTrait,
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    let mut exports = Vec::new();
    for member in &mut item.items {
        let TraitItem::Fn(method) = member else {
            continue;
        };
        let Some(attr) = method.attrs.iter().find(|attr| attr.path().is_ident("vox")) else {
            continue;
        };
        let options = TraitMethodOptions::from_meta(&parse_nested_meta(attr)?)?;
        let method_ident = &method.sig.ident;
        let vox_name = options
            .name
            .clone()
            .unwrap_or_else(|| method_ident.to_string());
        let lowered_by = options.lowered_by;
        let purity = options.purity.tokens();
        let return_type = match &method.sig.output {
            ReturnType::Default => "()".to_owned(),
            ReturnType::Type(_, ty) => ty.to_token_stream().to_string(),
        };

        let mut parameters = Vec::new();
        for input in &mut method.sig.inputs {
            match input {
                FnArg::Receiver(_) => {}
                FnArg::Typed(parameter) => {
                    let name = match &*parameter.pat {
                        Pat::Ident(ident) => ident.ident.to_string(),
                        other => {
                            return Err(Error::new_spanned(
                                other,
                                "vox trait method parameters must use simple identifiers",
                            ));
                        }
                    };
                    let has_default = take_default_marker(&mut parameter.attrs)?;
                    let ty_text = parameter.ty.to_token_stream().to_string();
                    parameters.push(quote! {
                        ::voxlib_sdk::external_export::ExportedFunctionParameter {
                            name: #name,
                            rust_type: #ty_text,
                            has_default: #has_default,
                        }
                    });
                }
            }
        }

        exports.push(quote! {
            ::voxlib_sdk::external_export::inventory::submit! {
                ::voxlib_sdk::external_export::ExportedTraitMethodRegistration {
                    trait_vox_name: #trait_vox_name,
                    rust_name: stringify!(#method_ident),
                    vox_name: #vox_name,
                    lowered_by: #lowered_by,
                    purity: #purity,
                    parameters: &[#(#parameters),*],
                    return_rust_type: #return_type,
                    order: line!(),
                }
            }
        });

        let mut method_doc = extract_rust_doc_comments(&method.attrs);
        if let Some(d) = &options.doc {
            method_doc = d.clone();
        }
        if !method_doc.is_empty() {
            exports.push(quote! {
                ::voxlib_sdk::external_export::inventory::submit! {
                    ::voxlib_sdk::external_export::ExportedDocstringRegistration {
                        vox_name: #vox_name,
                        doc: #method_doc,
                        order: line!(),
                    }
                }
            });
        }
    }
    Ok(exports)
}

fn strip_nested_vox_attrs_from_trait(item: &mut ItemTrait) {
    for member in &mut item.items {
        let TraitItem::Fn(method) = member else {
            continue;
        };
        method.attrs.retain(|attr| !attr.path().is_ident("vox"));
    }
}

fn take_default_marker(attrs: &mut Vec<Attribute>) -> syn::Result<bool> {
    let mut has_default = false;
    let mut retained = Vec::with_capacity(attrs.len());
    for attr in attrs.drain(..) {
        if !attr.path().is_ident("vox") {
            retained.push(attr);
            continue;
        }

        for entry in parse_nested_meta(&attr)? {
            let Meta::Path(path) = entry else {
                return Err(Error::new_spanned(
                    attr,
                    "unsupported #[vox(...)] parameter option",
                ));
            };
            if path.is_ident("default") {
                has_default = true;
            } else {
                return Err(Error::new_spanned(
                    path,
                    "unsupported #[vox(...)] parameter option",
                ));
            }
        }
    }
    *attrs = retained;
    Ok(has_default)
}

struct FunctionOptions {
    name: Option<String>,
    purity: PurityValue,
    return_type_override: Option<LitStr>,
    doc: Option<String>,
}

impl FunctionOptions {
    fn from_meta(meta: &[Meta]) -> syn::Result<Self> {
        let mut options = Self {
            name: None,
            purity: PurityValue::Pure,
            return_type_override: None,
            doc: None,
        };

        for entry in meta {
            let Meta::NameValue(value) = entry else {
                return Err(Error::new_spanned(entry, "expected name = value"));
            };
            if value.path.is_ident("name") {
                options.name = Some(expect_lit_str(&value.value, "name")?.value());
                continue;
            }
            if value.path.is_ident("purity") {
                options.purity = PurityValue::parse(expect_lit_str(&value.value, "purity")?)?;
                continue;
            }
            if value.path.is_ident("return_type") {
                options.return_type_override =
                    Some(expect_lit_str(&value.value, "return_type")?.clone());
                continue;
            }
            if value.path.is_ident("doc") {
                options.doc = Some(expect_lit_str(&value.value, "doc")?.value());
                continue;
            }
            return Err(Error::new_spanned(value, "unsupported vox_fn option"));
        }

        Ok(options)
    }
}

struct TraitMethodOptions {
    name: Option<String>,
    lowered_by: String,
    purity: PurityValue,
    doc: Option<String>,
}

impl TraitMethodOptions {
    fn from_meta(meta: &[Meta]) -> syn::Result<Self> {
        let mut name = None;
        let mut lowered_by = None;
        let mut purity = PurityValue::Pure;
        let mut doc = None;

        for entry in meta {
            let Meta::NameValue(value) = entry else {
                return Err(Error::new_spanned(entry, "expected name = value"));
            };
            if value.path.is_ident("name") {
                name = Some(expect_lit_str(&value.value, "name")?.value());
                continue;
            }
            if value.path.is_ident("lowered_by") {
                lowered_by = Some(expect_ident_or_lit_str(&value.value, "lowered_by")?);
                continue;
            }
            if value.path.is_ident("purity") {
                purity = PurityValue::parse(expect_lit_str(&value.value, "purity")?)?;
                continue;
            }
            if value.path.is_ident("doc") {
                doc = Some(expect_lit_str(&value.value, "doc")?.value());
                continue;
            }
            return Err(Error::new_spanned(
                value,
                "unsupported trait method vox option",
            ));
        }

        Ok(Self {
            name,
            lowered_by: lowered_by.ok_or_else(|| {
                Error::new(
                    proc_macro2::Span::call_site(),
                    "trait method #[vox(...)] requires lowered_by",
                )
            })?,
            purity,
            doc,
        })
    }
}

fn expect_lit_str<'a>(expr: &'a syn::Expr, field: &str) -> syn::Result<&'a LitStr> {
    let syn::Expr::Lit(value) = expr else {
        return Err(Error::new_spanned(
            expr,
            format!("{field} expects a string literal"),
        ));
    };
    let syn::Lit::Str(value) = &value.lit else {
        return Err(Error::new_spanned(
            &value.lit,
            format!("{field} expects a string literal"),
        ));
    };
    Ok(value)
}

fn expect_ident_or_lit_str(expr: &syn::Expr, field: &str) -> syn::Result<String> {
    if let syn::Expr::Path(path) = expr {
        if let Some(ident) = path.path.get_ident() {
            return Ok(ident.to_string());
        }
    }
    if let syn::Expr::Lit(value) = expr {
        if let syn::Lit::Str(value) = &value.lit {
            return Ok(value.value());
        }
    }
    Err(Error::new_spanned(
        expr,
        format!("{field} expects an identifier or string literal"),
    ))
}

enum PurityValue {
    Pure,
    Evil,
}

impl PurityValue {
    fn parse(value: &LitStr) -> syn::Result<Self> {
        match value.value().as_str() {
            "pure" => Ok(Self::Pure),
            "evil" => Ok(Self::Evil),
            _ => Err(Error::new_spanned(
                value,
                "purity must be \"pure\" or \"evil\"",
            )),
        }
    }

    fn tokens(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Pure => quote!(::vox_core::host::Purity::Pure),
            Self::Evil => quote!(::vox_core::host::Purity::Evil),
        }
    }
}
