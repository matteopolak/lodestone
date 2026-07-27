//! Procedural derives for Lodestone protocol models.
//!
//! `#[mc(fixed = N)]` on `[u8; N]` or `Vec<u8>` fields writes exactly `N`
//! raw bytes with no length prefix. It takes precedence over the legacy
//! `[u8; 16]` UUID special-case, so `#[mc(fixed = 16)]` is raw bytes.
//!
//! `#[mc(decode_context = "Type")]` on a `Decode` container generates an
//! inherent `decode_with(r, ctx, decode_ctx: &Type)` method. Fields can opt into
//! that structural context with `#[mc(decode_with = "path::to::decoder")]`.
//!
//! `#[mc(bits(x = 26, y = 12, z = 26, order = "xyz"))]` on a `BlockPos` newtype
//! packs signed coordinates into one big-endian i64. The legacy field-level
//! `#[mc(bits = N, signed)]` form packs all non-skipped fields of a struct into
//! one big-endian i64 in source order.
//!
//! `#[mc(varint)]` on `Vec<i32>`-compatible fields keeps the normal length
//! prefix, but encodes each element as a VarInt.
//!
//! `#[mc(present_if = "previous_field != -1")]` / `#[mc(when = "...")]`
//! conditionally encodes/decodes a named struct field based on a prior named
//! field. When absent on decode the field is filled with `Default::default()`.
//! For `Option<T>` fields, the condition controls the wire presence directly:
//! present decodes `Some(T)` without an extra bool, absent decodes `None`.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{
    Attribute, BinOp, Data, DataEnum, DataStruct, DeriveInput, Expr, ExprLit, ExprPath, Field,
    Fields, GenericArgument, Generics, Ident, Lit, LitInt, LitStr, Path, Type, TypeArray,
    TypePath, parse_macro_input, parse_quote,
};

#[proc_macro_derive(Encode, attributes(mc))]
pub fn derive_encode(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_encode(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[proc_macro_derive(Decode, attributes(mc))]
pub fn derive_decode(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_decode(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[proc_macro_derive(Packet, attributes(mc))]
pub fn derive_packet(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_packet(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Clone)]
struct ContainerAttr {
    crate_path: Path,
    decode_context: Option<Type>,
    bits: Option<PositionBitsAttr>,
    enum_repr: EnumRepr,
    packet_name: Option<LitStr>,
    packet_state: Option<Ident>,
    packet_bound: Option<Ident>,
}

impl Default for ContainerAttr {
    fn default() -> Self {
        Self {
            crate_path: parse_quote!(::lodestone_core),
            decode_context: None,
            bits: None,
            enum_repr: EnumRepr::VarInt,
            packet_name: None,
            packet_state: None,
            packet_bound: None,
        }
    }
}

#[derive(Clone, Copy)]
enum EnumRepr {
    VarInt,
    U8,
    I32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VarEncoding {
    Fixed,
    VarInt,
    VarLong,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LenKind {
    VarInt,
    U8,
    I16,
}

#[derive(Clone)]
struct FieldAttr {
    var_encoding: VarEncoding,
    len_kind: LenKind,
    len_explicit: bool,
    decode_with: Option<Path>,
    present_if: Option<PresentIf>,
    fixed: Option<usize>,
    max: Option<usize>,
    since: Option<i32>,
    until: Option<i32>,
    bits: Option<BitsAttr>,
    remaining: bool,
    skip: bool,
}

#[derive(Clone, Copy)]
struct BitsAttr {
    width: usize,
    signed: bool,
}

#[derive(Clone)]
struct PresentIf {
    field: Ident,
    op: PresentIfOp,
    literal: Expr,
}

#[derive(Clone, Copy)]
enum PresentIfOp {
    Eq,
    Ne,
}

#[derive(Clone)]
struct PositionBitsAttr {
    x: usize,
    y: usize,
    z: usize,
    order: PositionBitsOrder,
}

#[derive(Clone, Copy)]
enum PositionBitsOrder {
    Xyz,
    Xzy,
}

#[derive(Clone, Copy)]
enum PositionCoord {
    X,
    Y,
    Z,
}

impl PositionBitsAttr {
    const fn width(&self, coord: PositionCoord) -> usize {
        match coord {
            PositionCoord::X => self.x,
            PositionCoord::Y => self.y,
            PositionCoord::Z => self.z,
        }
    }
}

impl PositionBitsOrder {
    const fn coords(self) -> [PositionCoord; 3] {
        match self {
            Self::Xyz => [PositionCoord::X, PositionCoord::Y, PositionCoord::Z],
            Self::Xzy => [PositionCoord::X, PositionCoord::Z, PositionCoord::Y],
        }
    }
}

impl PositionCoord {
    fn ident(self) -> Ident {
        match self {
            Self::X => format_ident!("x"),
            Self::Y => format_ident!("y"),
            Self::Z => format_ident!("z"),
        }
    }
}

impl Default for FieldAttr {
    fn default() -> Self {
        Self {
            var_encoding: VarEncoding::Fixed,
            len_kind: LenKind::VarInt,
            len_explicit: false,
            decode_with: None,
            present_if: None,
            fixed: None,
            max: None,
            since: None,
            until: None,
            bits: None,
            remaining: false,
            skip: false,
        }
    }
}

#[derive(Clone, Copy)]
enum ValueMode {
    OwnedOrField,
    RefBinding,
}

enum PredicateMode<'a> {
    Encode,
    Decode(&'a [(Ident, Ident)]),
}

impl FieldAttr {
    const fn has_predicate(&self) -> bool {
        self.since.is_some() || self.until.is_some() || self.present_if.is_some()
    }
}

fn expand_encode(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let container = parse_container_attrs(&input.attrs)?;
    let crate_path = &container.crate_path;
    let ident = &input.ident;
    let generics = add_trait_bounds(input.generics.clone(), crate_path, parse_quote!(Encode));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let body = match &input.data {
        Data::Struct(data) => encode_struct_body(crate_path, data, container.bits.as_ref())?,
        Data::Enum(data) => encode_enum_body(crate_path, data, container.enum_repr)?,
        Data::Union(data) => {
            return Err(syn::Error::new_spanned(
                data.union_token,
                "Encode cannot be derived for unions",
            ));
        }
    };

    Ok(quote! {
        impl #impl_generics #crate_path::Encode for #ident #ty_generics #where_clause {
            fn encode(&self, w: &mut #crate_path::Writer, ctx: #crate_path::Ctx) -> #crate_path::Result<()> {
                #body
                Ok(())
            }
        }
    })
}

fn expand_decode(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let container = parse_container_attrs(&input.attrs)?;
    let crate_path = &container.crate_path;
    let ident = &input.ident;
    let generics = add_trait_bounds(input.generics.clone(), crate_path, parse_quote!(Decode));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let body = match &input.data {
        Data::Struct(data) => decode_struct_body(
            crate_path,
            ident,
            data,
            container.decode_context.is_some(),
            container.bits.as_ref(),
        )?,
        Data::Enum(data) => decode_enum_body(
            crate_path,
            ident,
            data,
            container.enum_repr,
            container.decode_context.is_some(),
        )?,
        Data::Union(data) => {
            return Err(syn::Error::new_spanned(
                data.union_token,
                "Decode cannot be derived for unions",
            ));
        }
    };

    if let Some(decode_context) = &container.decode_context {
        return Ok(quote! {
            impl #impl_generics #ident #ty_generics #where_clause {
                pub fn decode_with(
                    r: &mut #crate_path::Reader<'_>,
                    ctx: #crate_path::Ctx,
                    decode_ctx: &#decode_context,
                ) -> #crate_path::Result<Self> {
                    #body
                }
            }
        });
    }

    Ok(quote! {
        impl #impl_generics #crate_path::Decode for #ident #ty_generics #where_clause {
            fn decode(r: &mut #crate_path::Reader<'_>, ctx: #crate_path::Ctx) -> #crate_path::Result<Self> {
                #body
            }
        }
    })
}

fn expand_packet(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let container = parse_container_attrs(&input.attrs)?;
    let crate_path = &container.crate_path;
    let ident = &input.ident;
    let name = container.packet_name.ok_or_else(|| {
        syn::Error::new_spanned(ident, "Packet requires #[mc(name = \"namespace:path\")]")
    })?;
    let state = container.packet_state.ok_or_else(|| {
        syn::Error::new_spanned(
            ident,
            "Packet requires #[mc(state = Handshaking|Status|Login|Configuration|Play)]",
        )
    })?;
    let bound = container.packet_bound.ok_or_else(|| {
        syn::Error::new_spanned(ident, "Packet requires #[mc(bound = Client|Server)]")
    })?;
    validate_packet_state(&state)?;
    validate_packet_bound(&bound)?;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics #crate_path::Packet for #ident #ty_generics #where_clause {
            const NAME: &'static str = #name;
            const STATE: #crate_path::State = #crate_path::State::#state;
            const BOUND: #crate_path::Bound = #crate_path::Bound::#bound;
        }
    })
}

fn validate_packet_state(state: &Ident) -> syn::Result<()> {
    match state.to_string().as_str() {
        "Handshaking" | "Status" | "Login" | "Configuration" | "Play" => Ok(()),
        other => Err(syn::Error::new_spanned(
            state,
            format!(
                "unsupported packet state `{other}`; expected Handshaking, Status, Login, Configuration, or Play"
            ),
        )),
    }
}

fn validate_packet_bound(bound: &Ident) -> syn::Result<()> {
    match bound.to_string().as_str() {
        "Client" | "Server" => Ok(()),
        other => Err(syn::Error::new_spanned(
            bound,
            format!("unsupported packet bound `{other}`; expected Client or Server"),
        )),
    }
}

fn add_trait_bounds(mut generics: Generics, crate_path: &Path, bound_ident: Ident) -> Generics {
    for param in generics.type_params_mut() {
        param.bounds.push(parse_quote!(#crate_path::#bound_ident));
    }
    generics
}

fn encode_struct_body(
    crate_path: &Path,
    data: &DataStruct,
    container_bits: Option<&PositionBitsAttr>,
) -> syn::Result<TokenStream2> {
    validate_fields(&data.fields)?;
    if let Some(bits) = container_bits {
        return encode_position_bits_struct(data, bits);
    }
    if let Some(bit_fields) = bitpack_fields(&data.fields)? {
        return encode_bitpack_struct(crate_path, &bit_fields);
    }
    let encoders = data
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let attrs = parse_field_attrs(&field.attrs)?;
            let access = field_access(field, index);
            encode_field(crate_path, field, &attrs, access, ValueMode::OwnedOrField)
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(quote! { #(#encoders)* })
}

fn decode_struct_body(
    crate_path: &Path,
    ident: &Ident,
    data: &DataStruct,
    has_decode_context: bool,
    container_bits: Option<&PositionBitsAttr>,
) -> syn::Result<TokenStream2> {
    validate_fields(&data.fields)?;
    if let Some(bits) = container_bits {
        return decode_position_bits_struct(data, bits);
    }
    if let Some(bit_fields) = bitpack_fields(&data.fields)? {
        return decode_bitpack_struct(&bit_fields);
    }
    match &data.fields {
        Fields::Named(fields) => {
            let mut prior_bindings = Vec::<(Ident, Ident)>::new();
            let mut decoded = Vec::new();
            let mut initializers = Vec::new();
            for (index, field) in fields.named.iter().enumerate() {
                    let name = field.ident.as_ref().expect("named field has ident");
                    let binding = format_ident!("__mc_field_{index}");
                    let attrs = parse_field_attrs(&field.attrs)?;
                    let value = decode_field(
                        crate_path,
                        field,
                        &attrs,
                        has_decode_context,
                        &prior_bindings,
                    )?;
                    decoded.push(quote! { let #binding = #value; });
                    initializers.push(quote! { #name: #binding });
                    prior_bindings.push((name.clone(), binding));
            }
            Ok(quote! {
                #(#decoded)*
                Ok(Self { #(#initializers),* })
            })
        }
        Fields::Unnamed(fields) => {
            let decoded = fields
                .unnamed
                .iter()
                .map(|field| {
                    let attrs = parse_field_attrs(&field.attrs)?;
                    decode_field(crate_path, field, &attrs, has_decode_context, &[])
                })
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(quote! { Ok(Self( #(#decoded),* )) })
        }
        Fields::Unit => Ok(quote! {
            let _ = stringify!(#ident);
            Ok(Self)
        }),
    }
}

fn encode_enum_body(
    crate_path: &Path,
    data: &DataEnum,
    enum_repr: EnumRepr,
) -> syn::Result<TokenStream2> {
    let mut next_discriminant = 0_i64;
    let arms = data
        .variants
        .iter()
        .map(|variant| {
            validate_fields(&variant.fields)?;
            reject_present_if_on_variant_fields(&variant.fields)?;
            let discriminant = variant_discriminant(
                variant.discriminant.as_ref().map(|(_, expr)| expr),
                &mut next_discriminant,
            )?;
            let variant_ident = &variant.ident;
            validate_enum_discriminant(enum_repr, discriminant, variant_ident)?;
            let write_discriminant = encode_discriminant(crate_path, enum_repr, discriminant)?;
            match &variant.fields {
                Fields::Unit => Ok(quote! {
                    Self::#variant_ident => {
                        #write_discriminant
                    }
                }),
                Fields::Unnamed(fields) => {
                    let bindings = (0..fields.unnamed.len())
                        .map(|index| format_ident!("field_{index}"))
                        .collect::<Vec<_>>();
                    let encoders = fields
                        .unnamed
                        .iter()
                        .zip(bindings.iter())
                        .map(|(field, binding)| {
                            let attrs = parse_field_attrs(&field.attrs)?;
                            encode_field(
                                crate_path,
                                field,
                                &attrs,
                                quote!(#binding),
                                ValueMode::RefBinding,
                            )
                        })
                        .collect::<syn::Result<Vec<_>>>()?;
                    Ok(quote! {
                        Self::#variant_ident( #(#bindings),* ) => {
                            #write_discriminant
                            #(#encoders)*
                        }
                    })
                }
                Fields::Named(fields) => {
                    let bindings = fields
                        .named
                        .iter()
                        .map(|field| field.ident.clone().expect("named field has ident"))
                        .collect::<Vec<_>>();
                    let encoders = fields
                        .named
                        .iter()
                        .zip(bindings.iter())
                        .map(|(field, binding)| {
                            let attrs = parse_field_attrs(&field.attrs)?;
                            encode_field(
                                crate_path,
                                field,
                                &attrs,
                                quote!(#binding),
                                ValueMode::RefBinding,
                            )
                        })
                        .collect::<syn::Result<Vec<_>>>()?;
                    Ok(quote! {
                        Self::#variant_ident { #(#bindings),* } => {
                            #write_discriminant
                            #(#encoders)*
                        }
                    })
                }
            }
        })
        .collect::<syn::Result<Vec<_>>>()?;

    Ok(quote! { match self { #(#arms),* } })
}

fn decode_enum_body(
    crate_path: &Path,
    ident: &Ident,
    data: &DataEnum,
    enum_repr: EnumRepr,
    has_decode_context: bool,
) -> syn::Result<TokenStream2> {
    let read_discriminant = decode_discriminant(crate_path, enum_repr);
    let mut next_discriminant = 0_i64;
    let arms = data
        .variants
        .iter()
        .map(|variant| {
            validate_fields(&variant.fields)?;
            reject_present_if_on_variant_fields(&variant.fields)?;
            let discriminant = variant_discriminant(
                variant.discriminant.as_ref().map(|(_, expr)| expr),
                &mut next_discriminant,
            )?;
            let variant_ident = &variant.ident;
            validate_enum_discriminant(enum_repr, discriminant, variant_ident)?;
            match &variant.fields {
                Fields::Unit => Ok(quote! { #discriminant => Ok(Self::#variant_ident) }),
                Fields::Unnamed(fields) => {
                    let decoded = fields
                        .unnamed
                        .iter()
                        .map(|field| {
                            let attrs = parse_field_attrs(&field.attrs)?;
                            decode_field(crate_path, field, &attrs, has_decode_context, &[])
                        })
                        .collect::<syn::Result<Vec<_>>>()?;
                    Ok(quote! { #discriminant => Ok(Self::#variant_ident( #(#decoded),* )) })
                }
                Fields::Named(fields) => {
                    let decoded = fields
                        .named
                        .iter()
                        .map(|field| {
                            let name = field.ident.as_ref().expect("named field has ident");
                            let attrs = parse_field_attrs(&field.attrs)?;
                            let value =
                                decode_field(crate_path, field, &attrs, has_decode_context, &[])?;
                            Ok(quote! { #name: #value })
                        })
                        .collect::<syn::Result<Vec<_>>>()?;
                    Ok(quote! { #discriminant => Ok(Self::#variant_ident { #(#decoded),* }) })
                }
            }
        })
        .collect::<syn::Result<Vec<_>>>()?;

    Ok(quote! {
        let discriminant = { #read_discriminant };
        match discriminant {
            #(#arms,)*
            value => Err(#crate_path::Error::InvalidEnumVariant { name: stringify!(#ident), value: value as i32 }),
        }
    })
}

fn field_access(field: &Field, index: usize) -> TokenStream2 {
    field.ident.as_ref().map_or_else(
        || {
            let index = syn::Index::from(index);
            quote!(self.#index)
        },
        |ident| quote!(self.#ident),
    )
}

fn encode_field(
    crate_path: &Path,
    field: &Field,
    attrs: &FieldAttr,
    value: TokenStream2,
    mode: ValueMode,
) -> syn::Result<TokenStream2> {
    if attrs.skip {
        return Ok(TokenStream2::new());
    }
    let encoded = if attrs.present_if.is_some() && option_element(&field.ty).is_some() {
        encode_conditional_option(crate_path, field, attrs, value)?
    } else {
        encode_value(crate_path, &field.ty, attrs, value, mode)?
    };
    Ok(wrap_field_predicate(attrs, encoded, PredicateMode::Encode))
}

fn decode_field(
    crate_path: &Path,
    field: &Field,
    attrs: &FieldAttr,
    has_decode_context: bool,
    prior_bindings: &[(Ident, Ident)],
) -> syn::Result<TokenStream2> {
    if attrs.skip {
        return Ok(quote!(::core::default::Default::default()));
    }
    if attrs.decode_with.is_some() && !has_decode_context {
        return Err(syn::Error::new_spanned(
            field,
            "#[mc(decode_with = ...)] requires a container #[mc(decode_context = \"...\")]",
        ));
    }
    let decoded = if attrs.present_if.is_some() && option_element(&field.ty).is_some() {
        decode_conditional_option(crate_path, &field.ty, attrs)?
    } else {
        decode_value(crate_path, &field.ty, attrs)?
    };
    if attrs.has_predicate() {
        let condition = field_condition(attrs, PredicateMode::Decode(prior_bindings));
        Ok(quote! {
            if #condition {
                #decoded
            } else {
                ::core::default::Default::default()
            }
        })
    } else {
        Ok(decoded)
    }
}

fn encode_conditional_option(
    crate_path: &Path,
    field: &Field,
    attrs: &FieldAttr,
    value: TokenStream2,
) -> syn::Result<TokenStream2> {
    let element = option_element(&field.ty).expect("option checked by caller");
    let mut inner_attrs = attrs.clone();
    inner_attrs.present_if = None;
    let encoded = encode_value(
        crate_path,
        element,
        &inner_attrs,
        quote!(conditional_value),
        ValueMode::RefBinding,
    )?;
    let name = field_name(field, 0);
    Ok(quote! {
        match #value.as_ref() {
            ::core::option::Option::Some(conditional_value) => {
                #encoded
            }
            ::core::option::Option::None => {
                return Err(#crate_path::Error::Custom(
                    ::std::format!(
                        "conditional field {} is required when its predicate is true",
                        #name
                    )
                ));
            }
        }
    })
}

fn decode_conditional_option(
    crate_path: &Path,
    ty: &Type,
    attrs: &FieldAttr,
) -> syn::Result<TokenStream2> {
    let element = option_element(ty).expect("option checked by caller");
    let mut inner_attrs = attrs.clone();
    inner_attrs.present_if = None;
    let decoded = decode_value(crate_path, element, &inner_attrs)?;
    Ok(quote! { ::core::option::Option::Some(#decoded) })
}

fn encode_value(
    crate_path: &Path,
    ty: &Type,
    attrs: &FieldAttr,
    value: TokenStream2,
    mode: ValueMode,
) -> syn::Result<TokenStream2> {
    if attrs.fixed.is_some() {
        return encode_fixed_bytes(crate_path, ty, attrs, value, mode);
    }

    if attrs.remaining {
        validate_vec_u8(ty, "#[mc(remaining)] is only valid on Vec<u8> fields")?;
        return Ok(quote! { w.bytes(#value.as_slice()); });
    }

    match attrs.var_encoding {
        VarEncoding::VarInt => {
            if vec_element(ty).is_some() {
                return encode_vec(crate_path, ty, attrs, value);
            }
            validate_var_integer(ty, "varint")?;
            let value = copy_value(value, mode);
            Ok(quote! { w.var_i32(#value as i32); })
        }
        VarEncoding::VarLong => {
            validate_varlong_integer(ty)?;
            let value = copy_value(value, mode);
            Ok(quote! { w.var_i64(#value as i64); })
        }
        VarEncoding::Fixed => match primitive_method(ty) {
            Some(method) => {
                let method = format_ident!("{method}");
                let value = copy_value(value, mode);
                Ok(quote! { w.#method(#value); })
            }
            None if is_string(ty) => encode_string(crate_path, attrs, value),
            None if vec_element(ty).is_some() => encode_vec(crate_path, ty, attrs, value),
            None if is_uuid(ty) => {
                let value = copy_value(value, mode);
                Ok(quote! { w.uuid(#value); })
            }
            None => {
                let receiver = match mode {
                    ValueMode::OwnedOrField => quote!(&#value),
                    ValueMode::RefBinding => value,
                };
                Ok(quote! { #crate_path::Encode::encode(#receiver, w, ctx)?; })
            }
        },
    }
}

fn decode_value(crate_path: &Path, ty: &Type, attrs: &FieldAttr) -> syn::Result<TokenStream2> {
    if let Some(decode_with) = &attrs.decode_with {
        return Ok(quote! { #decode_with(r, ctx, decode_ctx)? });
    }

    if attrs.fixed.is_some() {
        return decode_fixed_bytes(ty, attrs);
    }

    if attrs.remaining {
        validate_vec_u8(ty, "#[mc(remaining)] is only valid on Vec<u8> fields")?;
        return Ok(quote! {{
            let len = r.remaining();
            r.bytes(len)?.to_vec()
        }});
    }

    match attrs.var_encoding {
        VarEncoding::VarInt => {
            if vec_element(ty).is_some() {
                return decode_vec(crate_path, ty, attrs);
            }
            validate_var_integer(ty, "varint")?;
            Ok(quote! { r.var_i32()? as _ })
        }
        VarEncoding::VarLong => {
            validate_varlong_integer(ty)?;
            Ok(quote! { r.var_i64()? as _ })
        }
        VarEncoding::Fixed => match primitive_method(ty) {
            Some(method) => {
                let method = format_ident!("{method}");
                Ok(quote! { r.#method()? })
            }
            None if is_string(ty) => decode_string(crate_path, attrs),
            None if vec_element(ty).is_some() => decode_vec(crate_path, ty, attrs),
            None if is_uuid(ty) => Ok(quote! { r.uuid()? }),
            None => Ok(quote! { <#ty as #crate_path::Decode>::decode(r, ctx)? }),
        },
    }
}

fn encode_fixed_bytes(
    crate_path: &Path,
    ty: &Type,
    attrs: &FieldAttr,
    value: TokenStream2,
    mode: ValueMode,
) -> syn::Result<TokenStream2> {
    let fixed = attrs.fixed.expect("checked by caller");
    if array_u8_len(ty).is_some() {
        let bytes = match mode {
            ValueMode::OwnedOrField => quote!(&#value),
            ValueMode::RefBinding => value,
        };
        Ok(quote! { w.bytes(#bytes); })
    } else if vec_element(ty).is_some_and(is_u8) {
        let len = quote!(#value.len());
        let check = fixed_len_check(crate_path, len, fixed);
        Ok(quote! {
            #check
            w.bytes(#value.as_slice());
        })
    } else {
        Err(syn::Error::new_spanned(
            ty,
            "#[mc(fixed = ...)] is only valid on [u8; N] and Vec<u8> fields",
        ))
    }
}

fn decode_fixed_bytes(ty: &Type, attrs: &FieldAttr) -> syn::Result<TokenStream2> {
    let fixed = attrs.fixed.expect("checked by caller");
    if array_u8_len(ty).is_some() {
        Ok(quote! {{
            let bytes = r.bytes(#fixed)?;
            let mut value = [0_u8; #fixed];
            value.copy_from_slice(bytes.as_ref());
            value
        }})
    } else if vec_element(ty).is_some_and(is_u8) {
        Ok(quote! { r.bytes(#fixed)?.to_vec() })
    } else {
        Err(syn::Error::new_spanned(
            ty,
            "#[mc(fixed = ...)] is only valid on [u8; N] and Vec<u8> fields",
        ))
    }
}

fn encode_string(
    crate_path: &Path,
    attrs: &FieldAttr,
    value: TokenStream2,
) -> syn::Result<TokenStream2> {
    let max = attrs.max.unwrap_or(usize::MAX);
    let check = limit_check(crate_path, quote!(#value.chars().count()), max);
    if attrs.len_kind == LenKind::VarInt {
        Ok(quote! {
            #check
            w.string(#value.as_str());
        })
    } else {
        let write_len = write_len(crate_path, attrs.len_kind, quote!(#value.len()))?;
        Ok(quote! {
            #check
            #write_len
            w.bytes(#value.as_bytes());
        })
    }
}

fn decode_string(crate_path: &Path, attrs: &FieldAttr) -> syn::Result<TokenStream2> {
    let max = attrs.max.unwrap_or(usize::MAX);
    if attrs.len_kind == LenKind::VarInt {
        Ok(quote! { r.string(#max)? })
    } else {
        let read_len = read_len(crate_path, attrs.len_kind)?;
        let check = limit_check(crate_path, quote!(value.chars().count()), max);
        Ok(quote! {{
            let len = { #read_len };
            let bytes = r.bytes(len)?;
            let value = ::core::str::from_utf8(bytes)
                .map_err(|_| #crate_path::Error::InvalidUtf8)?
                .to_owned();
            #check
            value
        }})
    }
}

fn encode_vec(
    crate_path: &Path,
    ty: &Type,
    attrs: &FieldAttr,
    value: TokenStream2,
) -> syn::Result<TokenStream2> {
    let element = vec_element(ty).expect("vec checked by caller");
    let max_check = attrs
        .max
        .map(|max| limit_check(crate_path, quote!(#value.len()), max))
        .unwrap_or_default();
    let write_len = write_len(crate_path, attrs.len_kind, quote!(#value.len()))?;

    if attrs.var_encoding == VarEncoding::VarInt {
        validate_var_integer(element, "varint")?;
        Ok(quote! {
            #max_check
            #write_len
            for item in #value.iter() {
                w.var_i32(*item as i32);
            }
        })
    } else if is_u8(element) {
        Ok(quote! {
            #max_check
            #write_len
            w.bytes(#value.as_slice());
        })
    } else {
        Ok(quote! {
            #max_check
            #write_len
            for item in #value.iter() {
                #crate_path::Encode::encode(item, w, ctx)?;
            }
        })
    }
}

fn decode_vec(crate_path: &Path, ty: &Type, attrs: &FieldAttr) -> syn::Result<TokenStream2> {
    let element = vec_element(ty).expect("vec checked by caller");
    let read_len = read_len(crate_path, attrs.len_kind)?;
    let max_check = attrs
        .max
        .map(|max| limit_check(crate_path, quote!(len), max))
        .unwrap_or_default();

    if attrs.var_encoding == VarEncoding::VarInt {
        validate_var_integer(element, "varint")?;
        Ok(quote! {{
            let len = { #read_len };
            #max_check
            let mut values = ::std::vec::Vec::with_capacity(len);
            for _ in 0..len {
                values.push(r.var_i32()? as _);
            }
            values
        }})
    } else if is_u8(element) {
        Ok(quote! {{
            let len = { #read_len };
            #max_check
            r.bytes(len)?.to_vec()
        }})
    } else {
        Ok(quote! {{
            let len = { #read_len };
            #max_check
            let mut values = ::std::vec::Vec::with_capacity(len);
            for _ in 0..len {
                values.push(<#element as #crate_path::Decode>::decode(r, ctx)?);
            }
            values
        }})
    }
}

struct BitField<'a> {
    field: &'a Field,
    index: usize,
    bits: BitsAttr,
}

fn encode_position_bits_struct(
    data: &DataStruct,
    bits: &PositionBitsAttr,
) -> syn::Result<TokenStream2> {
    let source = position_bits_source(&data.fields)?;
    let packers = position_bits_layout(bits)
        .into_iter()
        .map(|(coord, width, shift)| {
            let value = source.coord_access(coord);
            let mask = bit_mask(width);
            quote! {
                {
                    let raw = ((#value as i128) as u128 & (#mask as u128)) as u64;
                    packed |= raw << #shift;
                }
            }
        })
        .collect::<Vec<_>>();

    Ok(quote! {
        let mut packed: u64 = 0;
        #(#packers)*
        w.i64(packed as i64);
    })
}

fn decode_position_bits_struct(
    data: &DataStruct,
    bits: &PositionBitsAttr,
) -> syn::Result<TokenStream2> {
    let source = position_bits_source(&data.fields)?;
    let decoded = position_bits_layout(bits)
        .into_iter()
        .map(|(coord, width, shift)| {
            let ident = coord.ident();
            let mask = bit_mask(width);
            let sign_bit = 1_u64 << (width - 1);
            quote! {
                let #ident = {
                    let raw = (packed >> #shift) & #mask;
                    let value = if raw & #sign_bit != 0 {
                        raw as i128 - (1_i128 << #width)
                    } else {
                        raw as i128
                    };
                    value as _
                };
            }
        })
        .collect::<Vec<_>>();
    let construct = source.construct();

    Ok(quote! {
        let packed = r.i64()? as u64;
        #(#decoded)*
        Ok(#construct)
    })
}

enum PositionBitsSource<'a> {
    Newtype(&'a Type),
    Named,
}

impl PositionBitsSource<'_> {
    fn coord_access(&self, coord: PositionCoord) -> TokenStream2 {
        let ident = coord.ident();
        match self {
            Self::Newtype(_) => quote!(self.0.#ident),
            Self::Named => quote!(self.#ident),
        }
    }

    fn construct(&self) -> TokenStream2 {
        match self {
            Self::Newtype(ty) => quote!(Self(#ty { x, y, z })),
            Self::Named => quote!(Self { x, y, z }),
        }
    }
}

fn position_bits_source(fields: &Fields) -> syn::Result<PositionBitsSource<'_>> {
    match fields {
        Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
            Ok(PositionBitsSource::Newtype(&fields.unnamed[0].ty))
        }
        Fields::Unnamed(_) => Err(syn::Error::new_spanned(
            fields,
            "#[mc(bits(...))] tuple structs must wrap exactly one position field",
        )),
        Fields::Named(fields) => {
            if fields.named.len() != 3 {
                return Err(syn::Error::new_spanned(
                    fields,
                    "#[mc(bits(...))] named structs must contain exactly x, y, and z fields",
                ));
            }
            for expected in ["x", "y", "z"] {
                if !fields
                    .named
                    .iter()
                    .any(|field| field.ident.as_ref().is_some_and(|ident| ident == expected))
                {
                    return Err(syn::Error::new_spanned(
                        fields,
                        "#[mc(bits(...))] named structs must contain x, y, and z fields",
                    ));
                }
            }
            Ok(PositionBitsSource::Named)
        }
        Fields::Unit => Err(syn::Error::new_spanned(
            fields,
            "#[mc(bits(...))] requires a position newtype or named x/y/z fields",
        )),
    }
}

fn position_bits_layout(bits: &PositionBitsAttr) -> Vec<(PositionCoord, usize, usize)> {
    let ordered = bits.order.coords();
    let mut shift = ordered
        .iter()
        .map(|coord| bits.width(*coord))
        .sum::<usize>();
    ordered
        .iter()
        .map(|coord| {
            let width = bits.width(*coord);
            shift -= width;
            (*coord, width, shift)
        })
        .collect()
}

fn bitpack_fields(fields: &Fields) -> syn::Result<Option<Vec<BitField<'_>>>> {
    let parsed = fields
        .iter()
        .map(|field| parse_field_attrs(&field.attrs))
        .collect::<syn::Result<Vec<_>>>()?;
    let has_bits = parsed.iter().any(|attrs| attrs.bits.is_some());
    if !has_bits {
        return Ok(None);
    }

    let mut bit_fields = Vec::new();
    for (index, (field, attrs)) in fields.iter().zip(parsed.iter()).enumerate() {
        if attrs.skip {
            continue;
        }
        let Some(bits) = attrs.bits else {
            return Err(syn::Error::new_spanned(
                field,
                "bit-packed structs must annotate every non-skipped field with #[mc(bits = ...)]",
            ));
        };
        if attrs.since.is_some() || attrs.until.is_some() {
            return Err(syn::Error::new_spanned(
                field,
                "bit-packed fields cannot use #[mc(since = ...)] or #[mc(until = ...)]",
            ));
        }
        bit_fields.push(BitField { field, index, bits });
    }

    let total_bits: usize = bit_fields.iter().map(|field| field.bits.width).sum();
    if total_bits != 64 {
        return Err(syn::Error::new_spanned(
            fields,
            format!("bit-packed structs must contain exactly 64 bits; found {total_bits}"),
        ));
    }
    Ok(Some(bit_fields))
}

fn encode_bitpack_struct(crate_path: &Path, fields: &[BitField<'_>]) -> syn::Result<TokenStream2> {
    let mut shift = 64_usize;
    let mut encoders = Vec::new();
    for bit_field in fields {
        shift -= bit_field.bits.width;
        let field = bit_field.field;
        let width = bit_field.bits.width;
        let mask = bit_mask(width);
        let value = field_access(field, bit_field.index);
        let name = field_name(field, bit_field.index);
        let range_check = if bit_field.bits.signed {
            let min = -(1_i128 << (width - 1));
            let max = (1_i128 << (width - 1)) - 1;
            quote! {
                let value_i128 = #value as i128;
                if !(#min..=#max).contains(&value_i128) {
                    return Err(#crate_path::Error::Custom(
                        ::std::format!(
                            "bit field {}={} does not fit signed {}-bit range {}..={}",
                            #name,
                            value_i128,
                            #width,
                            #min,
                            #max
                        )
                    ));
                }
                let raw = (value_i128 as u128 & (#mask as u128)) as u64;
            }
        } else {
            let max = u128::from(mask);
            quote! {
                let value_u128 = #value as u128;
                if !(0..=#max).contains(&value_u128) {
                    return Err(#crate_path::Error::Custom(
                        ::std::format!(
                            "bit field {}={} does not fit unsigned {}-bit range 0..={}",
                            #name,
                            value_u128,
                            #width,
                            #max
                        )
                    ));
                }
                let raw = value_u128 as u64;
            }
        };
        encoders.push(quote! {
            {
                #range_check
                packed |= raw << #shift;
            }
        });
    }

    Ok(quote! {
        let mut packed: u64 = 0;
        #(#encoders)*
        w.i64(packed as i64);
    })
}

fn decode_bitpack_struct(fields: &[BitField<'_>]) -> syn::Result<TokenStream2> {
    let mut shift = 64_usize;
    let mut decoded = Vec::new();
    for bit_field in fields {
        shift -= bit_field.bits.width;
        let width = bit_field.bits.width;
        let mask = bit_mask(width);
        let value = if bit_field.bits.signed {
            let sign_bit = 1_u64 << (width - 1);
            quote! {{
                let raw = (packed >> #shift) & #mask;
                let value = if raw & #sign_bit != 0 {
                    raw as i128 - (1_i128 << #width)
                } else {
                    raw as i128
                };
                value as _
            }}
        } else {
            quote! { (((packed >> #shift) & #mask) as _) }
        };
        if let Some(name) = &bit_field.field.ident {
            decoded.push(quote! { #name: #value });
        } else {
            decoded.push(value);
        }
    }

    if fields
        .first()
        .is_some_and(|field| field.field.ident.is_some())
    {
        Ok(quote! {
            let packed = r.i64()? as u64;
            Ok(Self { #(#decoded),* })
        })
    } else {
        Ok(quote! {
            let packed = r.i64()? as u64;
            Ok(Self( #(#decoded),* ))
        })
    }
}

fn bit_mask(width: usize) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1_u64 << width) - 1
    }
}

fn field_name(field: &Field, index: usize) -> String {
    field
        .ident
        .as_ref()
        .map_or_else(|| index.to_string(), Ident::to_string)
}

fn read_len(crate_path: &Path, kind: LenKind) -> syn::Result<TokenStream2> {
    let raw = match kind {
        LenKind::VarInt => quote!(r.var_i32()?),
        LenKind::U8 => quote!(r.u8()? as i32),
        LenKind::I16 => quote!(r.i16()? as i32),
    };
    Ok(quote! {{
        let raw_len: i32 = #raw;
        if raw_len < 0 {
            return Err(#crate_path::Error::LimitExceeded { limit: 0, actual: raw_len.unsigned_abs() as usize });
        }
        raw_len as usize
    }})
}

fn write_len(crate_path: &Path, kind: LenKind, len: TokenStream2) -> syn::Result<TokenStream2> {
    let limit = match kind {
        LenKind::VarInt => i32::MAX as usize,
        LenKind::U8 => u8::MAX as usize,
        LenKind::I16 => i16::MAX as usize,
    };
    let writer = match kind {
        LenKind::VarInt => quote!(w.var_i32(len as i32);),
        LenKind::U8 => quote!(w.u8(len as u8);),
        LenKind::I16 => quote!(w.i16(len as i16);),
    };
    Ok(quote! {{
        let len = #len;
        if len > #limit {
            return Err(#crate_path::Error::LimitExceeded { limit: #limit, actual: len });
        }
        #writer
    }})
}

fn limit_check(crate_path: &Path, actual: TokenStream2, max: usize) -> TokenStream2 {
    quote! {
        {
            let actual = #actual;
            if actual > #max {
                return Err(#crate_path::Error::LimitExceeded { limit: #max, actual });
            }
        }
    }
}

fn fixed_len_check(crate_path: &Path, actual: TokenStream2, fixed: usize) -> TokenStream2 {
    quote! {
        {
            let actual = #actual;
            if actual != #fixed {
                return Err(#crate_path::Error::LimitExceeded { limit: #fixed, actual });
            }
        }
    }
}

fn wrap_field_predicate(
    attrs: &FieldAttr,
    tokens: TokenStream2,
    mode: PredicateMode<'_>,
) -> TokenStream2 {
    if attrs.has_predicate() {
        let condition = field_condition(attrs, mode);
        quote! {
            if #condition {
                #tokens
            }
        }
    } else {
        tokens
    }
}

fn field_condition(attrs: &FieldAttr, mode: PredicateMode<'_>) -> TokenStream2 {
    let version = version_condition(attrs);
    let present = attrs
        .present_if
        .as_ref()
        .map(|present_if| present_if_condition(present_if, &mode))
        .unwrap_or_else(|| quote!(true));
    quote!(#version && #present)
}

fn version_condition(attrs: &FieldAttr) -> TokenStream2 {
    let since = attrs
        .since
        .map(|since| quote!(ctx.version >= #since))
        .unwrap_or_else(|| quote!(true));
    let until = attrs
        .until
        .map(|until| quote!(ctx.version <= #until))
        .unwrap_or_else(|| quote!(true));
    quote!(#since && #until)
}

fn present_if_condition(present_if: &PresentIf, mode: &PredicateMode<'_>) -> TokenStream2 {
    let field = &present_if.field;
    let lhs = match mode {
        PredicateMode::Encode => quote!(self.#field),
        PredicateMode::Decode(prior_bindings) => prior_bindings
            .iter()
            .find(|(name, _)| name == field)
            .map_or_else(|| quote!(#field), |(_, binding)| quote!(#binding)),
    };
    let literal = &present_if.literal;
    match present_if.op {
        PresentIfOp::Eq => quote!(#lhs == #literal),
        PresentIfOp::Ne => quote!(#lhs != #literal),
    }
}

fn copy_value(value: TokenStream2, mode: ValueMode) -> TokenStream2 {
    match mode {
        ValueMode::OwnedOrField => value,
        ValueMode::RefBinding => quote!(*#value),
    }
}

fn encode_discriminant(
    crate_path: &Path,
    enum_repr: EnumRepr,
    discriminant: i64,
) -> syn::Result<TokenStream2> {
    match enum_repr {
        EnumRepr::VarInt => Ok(quote! { w.var_i32(#discriminant as i32); }),
        EnumRepr::U8 => Ok(quote! { w.u8(#discriminant as u8); }),
        EnumRepr::I32 => Ok(quote! { w.i32(#discriminant as i32); }),
    }
    .map(|tokens| quote! { let _ = stringify!(#crate_path); #tokens })
}

fn validate_enum_discriminant(
    enum_repr: EnumRepr,
    discriminant: i64,
    span: &Ident,
) -> syn::Result<()> {
    match enum_repr {
        EnumRepr::VarInt => {
            if (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&discriminant) {
                Ok(())
            } else {
                Err(syn::Error::new_spanned(
                    span,
                    "varint enum discriminants must fit in i32",
                ))
            }
        }
        EnumRepr::I32 => {
            if (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&discriminant) {
                Ok(())
            } else {
                Err(syn::Error::new_spanned(
                    span,
                    "i32 enum discriminants must fit in i32",
                ))
            }
        }
        EnumRepr::U8 => {
            if (0..=i64::from(u8::MAX)).contains(&discriminant) {
                Ok(())
            } else {
                Err(syn::Error::new_spanned(
                    span,
                    "u8 enum discriminants must be in 0..=255",
                ))
            }
        }
    }
}

fn decode_discriminant(crate_path: &Path, enum_repr: EnumRepr) -> TokenStream2 {
    let read = match enum_repr {
        EnumRepr::VarInt => quote!(r.var_i32()? as i64),
        EnumRepr::U8 => quote!(r.u8()? as i64),
        EnumRepr::I32 => quote!(r.i32()? as i64),
    };
    quote! { let _ = stringify!(#crate_path); #read }
}

fn variant_discriminant(expr: Option<&Expr>, next: &mut i64) -> syn::Result<i64> {
    let value = match expr {
        Some(expr) => int_expr_value(expr)?,
        None => *next,
    };
    *next = value
        .checked_add(1)
        .ok_or_else(|| syn::Error::new(Span::call_site(), "enum discriminant overflow"))?;
    Ok(value)
}

fn int_expr_value(expr: &Expr) -> syn::Result<i64> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse(),
        Expr::Unary(unary) => {
            if matches!(unary.op, syn::UnOp::Neg(_))
                && let Expr::Lit(ExprLit {
                    lit: Lit::Int(value),
                    ..
                }) = unary.expr.as_ref()
            {
                let parsed = value.base10_parse::<i64>()?;
                return parsed.checked_neg().ok_or_else(|| {
                    syn::Error::new_spanned(expr, "enum discriminant is too small")
                });
            }
            Err(syn::Error::new_spanned(
                expr,
                "enum discriminants must be integer literals",
            ))
        }
        _ => Err(syn::Error::new_spanned(
            expr,
            "enum discriminants must be integer literals",
        )),
    }
}

fn parse_container_attrs(attrs: &[Attribute]) -> syn::Result<ContainerAttr> {
    let mut out = ContainerAttr::default();
    for attr in mc_attrs(attrs) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate_path") {
                let lit: LitStr = meta.value()?.parse()?;
                out.crate_path = syn::parse_str(&lit.value()).map_err(|err| {
                    syn::Error::new(lit.span(), format!("invalid crate_path: {err}"))
                })?;
                Ok(())
            } else if meta.path.is_ident("repr") {
                let lit: LitStr = meta.value()?.parse()?;
                out.enum_repr = match lit.value().as_str() {
                    "varint" => EnumRepr::VarInt,
                    "u8" => EnumRepr::U8,
                    "i32" => EnumRepr::I32,
                    other => {
                        return Err(syn::Error::new(
                            lit.span(),
                            format!("unsupported enum repr `{other}`; expected \"varint\", \"u8\", or \"i32\""),
                        ));
                    }
                };
                Ok(())
            } else if meta.path.is_ident("decode_context") {
                let lit: LitStr = meta.value()?.parse()?;
                out.decode_context = Some(syn::parse_str(&lit.value()).map_err(|err| {
                    syn::Error::new(lit.span(), format!("invalid decode_context: {err}"))
                })?);
                Ok(())
            } else if meta.path.is_ident("bits") {
                out.bits = Some(parse_position_bits(meta)?);
                Ok(())
            } else if meta.path.is_ident("name") {
                out.packet_name = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("state") {
                out.packet_state = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("bound") {
                out.packet_bound = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(syn::Error::new_spanned(meta.path, "unknown #[mc(...)] container attribute"))
            }
        })?;
    }
    Ok(out)
}

fn parse_field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttr> {
    let mut out = FieldAttr::default();
    for attr in mc_attrs(attrs) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("varint") {
                if out.var_encoding != VarEncoding::Fixed {
                    return Err(syn::Error::new_spanned(meta.path, "duplicate integer encoding attribute"));
                }
                out.var_encoding = VarEncoding::VarInt;
                Ok(())
            } else if meta.path.is_ident("varlong") {
                if out.var_encoding != VarEncoding::Fixed {
                    return Err(syn::Error::new_spanned(meta.path, "duplicate integer encoding attribute"));
                }
                out.var_encoding = VarEncoding::VarLong;
                Ok(())
            } else if meta.path.is_ident("len") {
                let lit: LitStr = meta.value()?.parse()?;
                out.len_explicit = true;
                out.len_kind = match lit.value().as_str() {
                    "varint" => LenKind::VarInt,
                    "u8" => LenKind::U8,
                    "i16" => LenKind::I16,
                    other => {
                        return Err(syn::Error::new(
                            lit.span(),
                            format!("unsupported length prefix `{other}`; expected \"varint\", \"u8\", or \"i16\""),
                        ));
                    }
                };
                Ok(())
            } else if meta.path.is_ident("max") {
                out.max = Some(parse_lit_usize(meta.value()?.parse()?)?);
                Ok(())
            } else if meta.path.is_ident("fixed") {
                out.fixed = Some(parse_lit_usize(meta.value()?.parse()?)?);
                Ok(())
            } else if meta.path.is_ident("decode_with") {
                let lit: LitStr = meta.value()?.parse()?;
                out.decode_with = Some(syn::parse_str(&lit.value()).map_err(|err| {
                    syn::Error::new(lit.span(), format!("invalid decode_with: {err}"))
                })?);
                Ok(())
            } else if meta.path.is_ident("present_if") || meta.path.is_ident("when") {
                let lit: LitStr = meta.value()?.parse()?;
                out.present_if = Some(parse_present_if(&lit)?);
                Ok(())
            } else if meta.path.is_ident("bits") {
                let width = parse_lit_usize(meta.value()?.parse()?)?;
                out.bits = Some(BitsAttr {
                    width,
                    signed: out.bits.is_some_and(|bits| bits.signed),
                });
                Ok(())
            } else if meta.path.is_ident("signed") {
                let width = out.bits.map(|bits| bits.width).unwrap_or(0);
                out.bits = Some(BitsAttr {
                    width,
                    signed: true,
                });
                Ok(())
            } else if meta.path.is_ident("since") {
                out.since = Some(parse_lit_i32(meta.value()?.parse()?)?);
                Ok(())
            } else if meta.path.is_ident("until") {
                out.until = Some(parse_lit_i32(meta.value()?.parse()?)?);
                Ok(())
            } else if meta.path.is_ident("remaining") {
                out.remaining = true;
                Ok(())
            } else if meta.path.is_ident("skip") {
                out.skip = true;
                Ok(())
            } else {
                Err(syn::Error::new_spanned(meta.path, "unknown #[mc(...)] field attribute"))
            }
        })?;
    }
    if out.remaining && out.skip {
        return Err(syn::Error::new(
            Span::call_site(),
            "#[mc(remaining)] cannot be combined with #[mc(skip)]",
        ));
    }
    if out.present_if.is_some() && out.skip {
        return Err(syn::Error::new(
            Span::call_site(),
            "#[mc(present_if = ...)] cannot be combined with #[mc(skip)]",
        ));
    }
    if out.fixed.is_some() {
        if out.remaining {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[mc(fixed = ...)] cannot be combined with #[mc(remaining)]",
            ));
        }
        if out.len_explicit {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[mc(fixed = ...)] cannot be combined with #[mc(len = ...)]",
            ));
        }
        if out.max.is_some() {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[mc(fixed = ...)] cannot be combined with #[mc(max = ...)]",
            ));
        }
        if out.var_encoding != VarEncoding::Fixed {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[mc(fixed = ...)] cannot be combined with varint or varlong",
            ));
        }
    }
    if let Some(bits) = out.bits {
        if bits.width == 0 {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[mc(bits = ...)] must be greater than zero",
            ));
        }
        if bits.width > 64 {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[mc(bits = ...)] must be at most 64",
            ));
        }
        if out.fixed.is_some()
            || out.remaining
            || out.len_explicit
            || out.max.is_some()
            || out.var_encoding != VarEncoding::Fixed
            || out.decode_with.is_some()
            || out.present_if.is_some()
            || out.skip
            || out.since.is_some()
            || out.until.is_some()
        {
            return Err(syn::Error::new(
                Span::call_site(),
                "#[mc(bits = ...)] cannot be combined with other wire-format field attributes",
            ));
        }
    }
    Ok(out)
}

fn parse_position_bits(meta: syn::meta::ParseNestedMeta<'_>) -> syn::Result<PositionBitsAttr> {
    let mut x = None;
    let mut y = None;
    let mut z = None;
    let mut order = None;

    meta.parse_nested_meta(|nested| {
        if nested.path.is_ident("x") {
            x = Some(parse_lit_usize(nested.value()?.parse()?)?);
            Ok(())
        } else if nested.path.is_ident("y") {
            y = Some(parse_lit_usize(nested.value()?.parse()?)?);
            Ok(())
        } else if nested.path.is_ident("z") {
            z = Some(parse_lit_usize(nested.value()?.parse()?)?);
            Ok(())
        } else if nested.path.is_ident("order") {
            let lit: LitStr = nested.value()?.parse()?;
            order = Some(match lit.value().as_str() {
                "xyz" => PositionBitsOrder::Xyz,
                "xzy" => PositionBitsOrder::Xzy,
                other => {
                    return Err(syn::Error::new(
                        lit.span(),
                        format!(
                            "unsupported bit position order `{other}`; expected \"xyz\" or \"xzy\""
                        ),
                    ));
                }
            });
            Ok(())
        } else {
            Err(syn::Error::new_spanned(
                nested.path,
                "unknown #[mc(bits(...))] attribute",
            ))
        }
    })?;

    let bits = PositionBitsAttr {
        x: x.ok_or_else(|| syn::Error::new(Span::call_site(), "#[mc(bits(...))] requires x"))?,
        y: y.ok_or_else(|| syn::Error::new(Span::call_site(), "#[mc(bits(...))] requires y"))?,
        z: z.ok_or_else(|| syn::Error::new(Span::call_site(), "#[mc(bits(...))] requires z"))?,
        order: order
            .ok_or_else(|| syn::Error::new(Span::call_site(), "#[mc(bits(...))] requires order"))?,
    };

    for (name, width) in [("x", bits.x), ("y", bits.y), ("z", bits.z)] {
        if width == 0 {
            return Err(syn::Error::new(
                Span::call_site(),
                format!("#[mc(bits(...))] {name} width must be greater than zero"),
            ));
        }
        if width > 64 {
            return Err(syn::Error::new(
                Span::call_site(),
                format!("#[mc(bits(...))] {name} width must be at most 64"),
            ));
        }
    }

    let total = bits.x + bits.y + bits.z;
    if total != 64 {
        return Err(syn::Error::new(
            Span::call_site(),
            format!("#[mc(bits(...))] widths must sum to 64 bits; found {total}"),
        ));
    }

    Ok(bits)
}

fn mc_attrs(attrs: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attrs.iter().filter(|attr| attr.path().is_ident("mc"))
}

fn parse_lit_usize(lit: LitInt) -> syn::Result<usize> {
    lit.base10_parse()
}

fn parse_lit_i32(lit: LitInt) -> syn::Result<i32> {
    lit.base10_parse()
}

fn parse_present_if(lit: &LitStr) -> syn::Result<PresentIf> {
    let expr: Expr = syn::parse_str(&lit.value())
        .map_err(|err| syn::Error::new(lit.span(), format!("invalid present_if predicate: {err}")))?;
    let Expr::Binary(binary) = expr else {
        return Err(syn::Error::new(
            lit.span(),
            "#[mc(present_if = ...)] expects `previous_field == literal` or `previous_field != literal`",
        ));
    };
    let field = present_if_field(binary.left.as_ref(), lit.span())?;
    let op = match binary.op {
        BinOp::Eq(_) => PresentIfOp::Eq,
        BinOp::Ne(_) => PresentIfOp::Ne,
        _ => {
            return Err(syn::Error::new(
                lit.span(),
                "#[mc(present_if = ...)] only supports == and !=",
            ));
        }
    };
    if !present_if_literal(binary.right.as_ref()) {
        return Err(syn::Error::new(
            lit.span(),
            "#[mc(present_if = ...)] right-hand side must be an integer, bool, or string literal",
        ));
    }
    Ok(PresentIf {
        field,
        op,
        literal: (*binary.right).clone(),
    })
}

fn present_if_field(expr: &Expr, span: Span) -> syn::Result<Ident> {
    let Expr::Path(ExprPath { qself: None, path, .. }) = expr else {
        return Err(syn::Error::new(
            span,
            "#[mc(present_if = ...)] left-hand side must be a prior field name",
        ));
    };
    if path.segments.len() != 1 {
        return Err(syn::Error::new(
            span,
            "#[mc(present_if = ...)] left-hand side must be a prior field name",
        ));
    }
    Ok(path.segments[0].ident.clone())
}

fn present_if_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(_) | Lit::Bool(_) | Lit::Str(_),
            ..
        }) => true,
        Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => matches!(
            unary.expr.as_ref(),
            Expr::Lit(ExprLit {
                lit: Lit::Int(_),
                ..
            })
        ),
        _ => false,
    }
}

fn validate_fields(fields: &Fields) -> syn::Result<()> {
    let mut parsed = Vec::new();
    for field in fields {
        parsed.push(parse_field_attrs(&field.attrs)?);
    }
    let mut prior_names = Vec::<Ident>::new();
    for (index, attrs) in parsed.iter().enumerate() {
        let field = fields
            .iter()
            .nth(index)
            .expect("index from field iteration");
        validate_field_attr_usage(field, attrs)?;
        validate_present_if_field(fields, field, attrs, &prior_names)?;
        if attrs.remaining && index + 1 != parsed.len() {
            return Err(syn::Error::new_spanned(
                field,
                "#[mc(remaining)] is only valid on the final field",
            ));
        }
        if let Some(ident) = &field.ident {
            prior_names.push(ident.clone());
        }
    }
    Ok(())
}

fn validate_present_if_field(
    fields: &Fields,
    field: &Field,
    attrs: &FieldAttr,
    prior_names: &[Ident],
) -> syn::Result<()> {
    let Some(present_if) = &attrs.present_if else {
        return Ok(());
    };
    if !matches!(fields, Fields::Named(_)) {
        return Err(syn::Error::new_spanned(
            field,
            "#[mc(present_if = ...)] is only supported on named struct fields",
        ));
    }
    if !prior_names.iter().any(|ident| ident == &present_if.field) {
        return Err(syn::Error::new_spanned(
            field,
            format!(
                "#[mc(present_if = ...)] must reference a prior field; `{}` is not available",
                present_if.field
            ),
        ));
    }
    Ok(())
}

fn reject_present_if_on_variant_fields(fields: &Fields) -> syn::Result<()> {
    for field in fields {
        let attrs = parse_field_attrs(&field.attrs)?;
        if attrs.present_if.is_some() {
            return Err(syn::Error::new_spanned(
                field,
                "#[mc(present_if = ...)] is only supported on struct fields",
            ));
        }
    }
    Ok(())
}

fn validate_field_attr_usage(field: &Field, attrs: &FieldAttr) -> syn::Result<()> {
    if let Some(fixed) = attrs.fixed {
        validate_fixed_field(field, fixed)?;
    }
    if attrs.remaining {
        validate_vec_u8(
            &field.ty,
            "#[mc(remaining)] is only valid on Vec<u8> fields",
        )?;
    }
    if attrs.max.is_some() && !is_string(&field.ty) && vec_element(&field.ty).is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "#[mc(max = ...)] is only valid on String and Vec<T> fields",
        ));
    }
    if attrs.len_explicit && !is_string(&field.ty) && vec_element(&field.ty).is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "#[mc(len = ...)] is only valid on String and Vec<T> fields",
        ));
    }
    if attrs.bits.is_some() {
        validate_bit_field(field)?;
    }
    Ok(())
}

fn validate_fixed_field(field: &Field, fixed: usize) -> syn::Result<()> {
    if let Some(array_len) = array_u8_len(&field.ty) {
        if array_len == fixed {
            return Ok(());
        }
        return Err(syn::Error::new_spanned(
            field,
            format!("#[mc(fixed = {fixed})] does not match array length {array_len}"),
        ));
    }

    if vec_element(&field.ty).is_some_and(is_u8) {
        return Ok(());
    }

    Err(syn::Error::new_spanned(
        field,
        "#[mc(fixed = ...)] is only valid on [u8; N] and Vec<u8> fields",
    ))
}

fn validate_bit_field(field: &Field) -> syn::Result<()> {
    let Some(ident) = bare_type_ident(&field.ty) else {
        return Err(syn::Error::new_spanned(
            field,
            "#[mc(bits = ...)] requires an integer field",
        ));
    };
    match ident.as_str() {
        "i64" | "u64" | "i32" | "u32" | "i16" | "u16" | "i8" | "u8" => Ok(()),
        _ => Err(syn::Error::new_spanned(
            field,
            "#[mc(bits = ...)] requires an integer field",
        )),
    }
}

fn primitive_method(ty: &Type) -> Option<&'static str> {
    let ident = bare_type_ident(ty)?;
    match ident.as_str() {
        "u8" => Some("u8"),
        "i8" => Some("i8"),
        "u16" => Some("u16"),
        "i16" => Some("i16"),
        "u32" => Some("u32"),
        "i32" => Some("i32"),
        "u64" => Some("u64"),
        "i64" => Some("i64"),
        "f32" => Some("f32"),
        "f64" => Some("f64"),
        "bool" => Some("bool"),
        _ => None,
    }
}

fn validate_var_integer(ty: &Type, attr: &str) -> syn::Result<()> {
    let Some(ident) = bare_type_ident(ty) else {
        return Err(syn::Error::new_spanned(
            ty,
            format!("#[mc({attr})] requires an integer field"),
        ));
    };
    match ident.as_str() {
        "i32" | "u32" | "i16" | "u16" | "i8" | "u8" => Ok(()),
        _ => Err(syn::Error::new_spanned(
            ty,
            format!("#[mc({attr})] requires an i32-compatible integer field"),
        )),
    }
}

fn validate_varlong_integer(ty: &Type) -> syn::Result<()> {
    let Some(ident) = bare_type_ident(ty) else {
        return Err(syn::Error::new_spanned(
            ty,
            "#[mc(varlong)] requires an integer field",
        ));
    };
    match ident.as_str() {
        "i64" | "u64" | "i32" | "u32" | "i16" | "u16" | "i8" | "u8" => Ok(()),
        _ => Err(syn::Error::new_spanned(
            ty,
            "#[mc(varlong)] requires an i64-compatible integer field",
        )),
    }
}

fn bare_type_ident(ty: &Type) -> Option<String> {
    let Type::Path(TypePath {
        qself: None, path, ..
    }) = ty
    else {
        return None;
    };
    if path.segments.len() == 1 {
        Some(path.segments[0].ident.to_string())
    } else {
        None
    }
}

fn is_string(ty: &Type) -> bool {
    path_last_ident(ty).is_some_and(|ident| ident == "String")
}

fn is_uuid(ty: &Type) -> bool {
    if let Type::Array(TypeArray { elem, len, .. }) = ty {
        return is_u8(elem)
            && matches!(len, Expr::Lit(ExprLit { lit: Lit::Int(value), .. }) if value.base10_digits() == "16");
    }
    path_last_ident(ty).is_some_and(|ident| ident == "Uuid")
}

fn array_u8_len(ty: &Type) -> Option<usize> {
    let Type::Array(TypeArray { elem, len, .. }) = ty else {
        return None;
    };
    if !is_u8(elem) {
        return None;
    }
    match len {
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse().ok(),
        _ => None,
    }
}

fn path_last_ident(ty: &Type) -> Option<String> {
    let Type::Path(TypePath {
        qself: None, path, ..
    }) = ty
    else {
        return None;
    };
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn vec_element(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath {
        qself: None, path, ..
    }) = ty
    else {
        return None;
    };
    let segment = path.segments.last()?;
    if segment.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    }
}

fn option_element(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath {
        qself: None, path, ..
    }) = ty
    else {
        return None;
    };
    let segment = path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    }
}

fn validate_vec_u8(ty: &Type, message: &str) -> syn::Result<()> {
    match vec_element(ty) {
        Some(element) if is_u8(element) => Ok(()),
        _ => Err(syn::Error::new_spanned(ty, message)),
    }
}

fn is_u8(ty: &Type) -> bool {
    bare_type_ident(ty).is_some_and(|ident| ident == "u8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn encode_error(input: DeriveInput) -> String {
        expand_encode(&input)
            .expect_err("derive should fail")
            .to_string()
    }

    fn decode_error(input: DeriveInput) -> String {
        expand_decode(&input)
            .expect_err("derive should fail")
            .to_string()
    }

    fn packet_error(input: DeriveInput) -> String {
        expand_packet(&input)
            .expect_err("derive should fail")
            .to_string()
    }

    #[test]
    fn unknown_field_attribute_is_rejected() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(unknown)]
                value: u8,
            }
        };

        assert!(encode_error(input).contains("unknown #[mc(...)] field attribute"));
    }

    #[test]
    fn remaining_must_be_final_field() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(remaining)]
                bytes: Vec<u8>,
                tail: u8,
            }
        };

        assert!(encode_error(input).contains("#[mc(remaining)] is only valid on the final field"));
    }

    #[test]
    fn remaining_must_be_vec_u8() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(remaining)]
                bytes: Vec<i32>,
            }
        };

        assert!(encode_error(input).contains("#[mc(remaining)] is only valid on Vec<u8> fields"));
    }

    #[test]
    fn max_requires_string_or_vec() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(max = 5)]
                value: u8,
            }
        };

        assert!(
            encode_error(input)
                .contains("#[mc(max = ...)] is only valid on String and Vec<T> fields")
        );
    }

    #[test]
    fn len_requires_string_or_vec() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(len = "u8")]
                value: bool,
            }
        };

        assert!(
            encode_error(input)
                .contains("#[mc(len = ...)] is only valid on String and Vec<T> fields")
        );
    }

    #[test]
    fn packet_state_must_be_known() {
        let input: DeriveInput = parse_quote! {
            #[mc(name = "minecraft:test", state = Bogus, bound = Server)]
            struct Bad;
        };

        assert!(
            packet_error(input)
                .contains("unsupported packet state `Bogus`; expected Handshaking, Status, Login, Configuration, or Play")
        );
    }

    #[test]
    fn packet_bound_must_be_known() {
        let input: DeriveInput = parse_quote! {
            #[mc(name = "minecraft:test", state = Play, bound = Peer)]
            struct Bad;
        };

        assert!(
            packet_error(input)
                .contains("unsupported packet bound `Peer`; expected Client or Server")
        );
    }

    #[test]
    fn varint_enum_discriminants_must_fit_i32() {
        let input: DeriveInput = parse_quote! {
            enum Bad {
                TooLarge = 3_000_000_000,
            }
        };

        assert!(encode_error(input).contains("varint enum discriminants must fit in i32"));
    }

    #[test]
    fn fixed_array_length_mismatch_is_rejected() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(fixed = 3)]
                bytes: [u8; 4],
            }
        };

        assert!(encode_error(input).contains("#[mc(fixed = 3)] does not match array length 4"));
    }

    #[test]
    fn bitpacked_struct_requires_exactly_sixty_four_bits() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(bits = 26, signed)]
                x: i32,
                #[mc(bits = 12, signed)]
                y: i32,
            }
        };

        assert!(
            encode_error(input)
                .contains("bit-packed structs must contain exactly 64 bits; found 38")
        );
    }

    #[test]
    fn decode_with_requires_decode_context() {
        let input: DeriveInput = parse_quote! {
            struct Bad {
                #[mc(decode_with = "decode_field")]
                value: u8,
            }
        };

        assert!(
            decode_error(input)
                .contains("#[mc(decode_with = ...)] requires a container #[mc(decode_context")
        );
    }
}
