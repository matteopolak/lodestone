//! AST guard for keeping generated block states in `StateId` form.

use anyhow::{Context, Result, bail};
use quote::ToTokens;
use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{
    Attribute, Expr, ExprCall, ExprLit, ExprMethodCall, Field, FnArg, ImplItemFn, ItemConst,
    ItemEnum, ItemFn, ItemImpl, ItemMod, ItemStruct, Lit, Pat, PathArguments, Type,
};

const ROOTS: &[&str] = &[
    "crates/lodestone-worldgen/src",
    "crates/lodestone-server/src",
    "crates/lodestone-client/src",
    "crates/lodestone-model/src",
    "crates/lodestone-shell/src",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub file: String,
    pub line: usize,
    pub owner: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub files_scanned: usize,
    pub violations: Vec<Violation>,
}

impl Report {
    #[must_use]
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("worldgen state ids: {} files\n", self.files_scanned);
        if self.violations.is_empty() {
            out.push_str("worldgen state-id check passed\n");
        } else {
            for violation in &self.violations {
                out.push_str(&format!(
                    "VIOLATION {}:{} {} — {}\n",
                    violation.file, violation.line, violation.owner, violation.reason
                ));
            }
        }
        out
    }
}

#[derive(Default)]
struct Visitor {
    file: String,
    owner: Vec<String>,
    violations: Vec<Violation>,
}

impl Visitor {
    fn owner(&self) -> String {
        self.owner
            .last()
            .cloned()
            .unwrap_or_else(|| "module".to_owned())
    }

    fn check_type(&mut self, owner: &str, ty: &Type, line: usize) {
        if contains_state_text_type(ty)
            && (state_bearing_name(owner)
                || state_function_name(owner)
                || text_conversion_name(owner)
                || self.block_set_api(owner))
            && !self.boundary_symbol(owner)
            && !self.state_json_ingress_allowed_for(owner)
        {
            self.violations.push(Violation {
                file: self.file.clone(),
                line,
                owner: owner.to_owned(),
                reason: "state-bearing runtime value uses String; carry StateId".to_owned(),
            });
        }
    }

    fn check_field(&mut self, parent: &str, field: &Field) {
        let owner = field
            .ident
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| parent.to_owned());
        if self.boundary_symbol(parent) {
            return;
        }
        if contains_state_text_type(&field.ty)
            && (state_bearing_field(&owner, parent)
                || block_state_owner(parent)
                || block_set_storage(parent, &owner, &field.ty))
            && !self.boundary_symbol(&owner)
        {
            self.violations.push(Violation {
                file: self.file.clone(),
                line: field.span().start().line,
                owner,
                reason: "state-bearing runtime value uses String; carry StateId".to_owned(),
            });
        }
    }

    fn check_signature(&mut self, name: &str, item: &syn::Signature) {
        for input in &item.inputs {
            if let FnArg::Typed(arg) = input {
                let owner = pat_name(&arg.pat).unwrap_or_else(|| name.to_owned());
                let check_owner = if state_function_name(name)
                    || self.block_set_api(name)
                    || self.boundary_symbol(name)
                    || self.state_json_ingress_allowed_for(name)
                {
                    name
                } else {
                    &owner
                };
                self.check_type(check_owner, &arg.ty, arg.ty.span().start().line);
            }
        }
        if let syn::ReturnType::Type(_, ty) = &item.output {
            self.check_type(name, ty, ty.span().start().line);
        }
    }

    fn check_const(&mut self, item: &ItemConst) {
        if has_cfg_test(&item.attrs)
            || !contains_state_text_type(&item.ty)
            || !state_constant_name(&item.ident.to_string(), &self.file)
            || self.boundary_symbol(&item.ident.to_string())
        {
            return;
        }
        let Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) = item.expr.as_ref()
        else {
            return;
        };
        if !looks_like_block_state(value.value().as_str()) {
            return;
        }
        self.violations.push(Violation {
            file: self.file.clone(),
            line: item.span().start().line,
            owner: item.ident.to_string(),
            reason: "block-state constant is text; carry StateId".to_owned(),
        });
    }

    fn visit_function(&mut self, name: &str, sig: &syn::Signature, attrs: &[Attribute], body: &syn::Block) {
        if has_cfg_test(attrs) {
            return;
        }
        self.check_signature(name, sig);
        self.owner.push(name.to_owned());
        self.visit_block(body);
        self.owner.pop();
    }

    fn method_conversion_allowed(&self) -> bool {
        allowed_path(Path::new(&self.file))
            || self.state_json_ingress_allowed()
            || self.boundary_symbol(&self.owner())
    }

    fn state_json_ingress_allowed(&self) -> bool {
        self.state_json_ingress_allowed_for("")
    }

    fn block_set_api(&self, name: &str) -> bool {
        let file = self.file.replace('\\', "/");
        (file == "crates/lodestone-worldgen/src/feature/vegetation/ids.rs"
            && matches!(name, "member" | "base_id"))
            || (file == "crates/lodestone-worldgen/src/feature/vegetation/config.rs"
                && name == "resolve_block_set")
    }

    fn state_json_ingress_allowed_for(&self, symbol: &str) -> bool {
        let file = self.file.replace('\\', "/");
        (file == "crates/lodestone-worldgen/src/nether/mod.rs"
            && (self
                .owner
                .iter()
                .any(|owner| owner == "state_id_from_settings")
                || symbol == "state_id_from_settings"))
            || (file == "crates/lodestone-server/src/heavy_scene.rs"
                && (self
                    .owner
                    .iter()
                    .any(|owner| matches!(owner.as_str(), "setblock_state_id" | "apply_setblock_commands"))
                    || matches!(symbol, "setblock_state_id" | "apply_setblock_commands")))
            || (file == "crates/lodestone-server/src/worldgen_data.rs"
                && (self.owner.iter().any(|owner| {
                    matches!(owner.as_str(), "freeze_facts" | "survival_facts" | "canonical_state")
                }) || matches!(symbol, "freeze_facts" | "survival_facts" | "canonical_state")))
            || (file == "crates/lodestone-worldgen/src/feature/mod.rs"
                && (self.owner.iter().any(|owner| owner == "parse_ore_config")
                    || symbol == "parse_ore_config"))
            || (file == "crates/lodestone-worldgen/src/structure/template.rs"
                && (self
                    .owner
                    .iter()
                    .any(|owner| matches!(owner.as_str(), "parse_palette" | "parse_bound_state"))
                    || matches!(symbol, "parse_palette" | "parse_bound_state")))
    }

    fn boundary_symbol(&self, name: &str) -> bool {
        if boundary_symbol(name) {
            return true;
        }
        let symbol = name.to_ascii_lowercase();
        let file = self.file.replace('\\', "/");
        (file == "crates/lodestone-server/src/properties.rs"
            && matches!(name, "to_text" | "load_or_create" | "save"))
            || (file == "crates/lodestone-server/src/world_storage.rs"
                && symbol.ends_with("::invalidpackedstates"))
            || (file == "crates/lodestone-server/src/integrated.rs"
                && name == "set_resident_block_state_id")
            || (file == "crates/lodestone-server/src/chunk_owner_profile.rs"
                && name == "state")
            || (file == "crates/lodestone-worldgen/src/interner.rs"
                && matches!(name, "canonical_state_id" | "id_of"))
    }
}

impl<'ast> Visit<'ast> for Visitor {
    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        let parent = item.ident.to_string();
        match &item.fields {
            syn::Fields::Named(fields) => {
                for field in &fields.named {
                    self.check_field(&parent, field);
                }
            }
            syn::Fields::Unnamed(fields) => {
                for field in &fields.unnamed {
                    self.check_field(&parent, field);
                }
            }
            syn::Fields::Unit => {}
        }
        syn::visit::visit_item_struct(self, item);
    }

    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        self.check_const(item);
        syn::visit::visit_item_const(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        for variant in &item.variants {
            let parent = format!("{}::{}", item.ident, variant.ident);
            match &variant.fields {
                syn::Fields::Named(fields) => {
                    for field in &fields.named {
                        self.check_field(&parent, field);
                    }
                }
                syn::Fields::Unnamed(fields) => {
                    for field in &fields.unnamed {
                        self.check_field(&parent, field);
                    }
                }
                syn::Fields::Unit => {}
            }
        }
        syn::visit::visit_item_enum(self, item);
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.visit_function(&item.sig.ident.to_string(), &item.sig, &item.attrs, &item.block);
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if has_cfg_test(&item.attrs) {
            return;
        }
        syn::visit::visit_item_impl(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        self.visit_function(&item.sig.ident.to_string(), &item.sig, &item.attrs, &item.block);
    }

    fn visit_expr_method_call(&mut self, item: &'ast ExprMethodCall) {
        let method = item.method.to_string();
        if state_conversion_name(&method)
            && !self.method_conversion_allowed()
            && (matches!(method.as_str(), "name_of" | "canonical_state")
                || state_function_name(&method)
                || state_context_name(&self.owner()))
        {
            self.violations.push(Violation {
                file: self.file.clone(),
                line: item.method.span().start().line,
                owner: self.owner(),
                reason: format!("{method}() converts a runtime block state to text"),
            });
        }
        syn::visit::visit_expr_method_call(self, item);
    }

    fn visit_expr_call(&mut self, item: &'ast ExprCall) {
        let name = call_name(&item.func);
        if name == "state_id" && !self.method_conversion_allowed() {
            self.violations.push(Violation {
                file: self.file.clone(),
                line: item.func.span().start().line,
                owner: self.owner(),
                reason: "state_id() accepts block-state text; carry StateId".to_owned(),
            });
        }
        if matches!(
            name.as_str(),
            "from_state_str" | "canonical_state" | "name_of" | "to_text" | "state_name"
        )
            && is_state_conversion_call(&item.func, &name)
            && !self.method_conversion_allowed()
        {
            self.violations.push(Violation {
                file: self.file.clone(),
                line: item.func.span().start().line,
                owner: self.owner(),
                reason: format!("{name}() converts a runtime block state to text"),
            });
        }
        syn::visit::visit_expr_call(self, item);
    }
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr.meta.to_token_stream().to_string().contains("test"))
    })
}

fn pat_name(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(pat) => Some(pat.ident.to_string()),
        _ => None,
    }
}

fn state_bearing_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "state",
        "final_state",
        "palette",
        "spill",
        "carver",
        "new_state",
        "runtime_state",
        "grid_row",
        "states",
    ]
    .iter()
    .any(|part| name.contains(part))
}

fn state_bearing_field(field: &str, parent: &str) -> bool {
    if state_bearing_name(field) {
        return true;
    }
    let field = field.to_ascii_lowercase();
    if !matches!(field.as_str(), "event" | "from" | "to") {
        return false;
    }
    state_context_name(parent)
}

fn state_context_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "block",
        "tick",
        "reaction",
        "mutation",
        "state",
        "fluid",
        "fire",
        "gravity",
        "fall",
        "piston",
        "portal",
        "placement",
        "breaking",
        "drop",
        "effect",
        "light",
        "lightning",
        "collision",
    ]
    .iter()
    .any(|part| name.contains(part))
}

fn state_function_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("block_state")
        || name.contains("state_name")
        || name.contains("state_text")
        || matches!(
            name.as_str(),
            "block_at"
                | "state_id"
                | "base_name"
                | "base_id"
                | "to_text"
                | "placement_text"
                | "at_text"
                | "moving_piston_state"
                | "portal_state"
                | "end_portal_frame_state"
                | "is_randomly_ticking"
                | "is_growable_crop"
                | "is_sapling"
                | "is_leaves"
                | "leaves_should_decay"
                | "get_age"
                | "set_age"
                | "get_stage"
                | "set_stage"
                | "with_age"
        )
}

fn state_constant_name(name: &str, file: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let file = file.replace('\\', "/");
    if file.ends_with("/end/mod.rs") || name.ends_with("_name") {
        return false;
    }
    if [
        "biome",
        "entity",
        "item",
        "poi",
        "channel",
        "resource",
        "loot",
        "table",
        "font",
        "template",
        "pattern",
        "effect",
        "stat",
        "sound",
        "dimension",
        "feature",
        "structure",
    ]
    .iter()
    .any(|part| name.contains(part))
    {
        return false;
    }
    name.contains("state")
        || name.contains("block")
        || matches!(
            name.as_str(),
            "air"
                | "water"
                | "lava"
                | "stone"
                | "dirt"
                | "grass_block"
                | "sand"
                | "sandstone"
                | "gravel"
                | "ice"
                | "snow"
                | "basalt"
                | "netherrack"
                | "obsidian"
        )
        || looks_like_state_file(&file)
}

fn looks_like_state_file(file: &str) -> bool {
    let file = file.replace('\\', "/");
    file.contains("crates/lodestone-worldgen/src/feature/vegetation/")
        || file.ends_with("/feature/top_layer.rs")
        || file.ends_with("/structure/monument.rs")
        || matches!(
            file.rsplit('/').next(),
            Some("random_tick.rs")
                | Some("fluid.rs")
                | Some("growth_tick.rs")
                | Some("fire.rs")
                | Some("piston.rs")
                | Some("gravity_tick.rs")
        )
}

fn looks_like_block_state(value: &str) -> bool {
    value.starts_with("minecraft:")
        && !value.contains('/')
        && value
            .split_once(':')
            .is_some_and(|(_, name)| !name.is_empty() && !name.contains(' '))
}

fn block_set_storage(parent: &str, field: &str, ty: &Type) -> bool {
    let parent = parent.to_ascii_lowercase();
    let field = field.to_ascii_lowercase();
    if parent == "vegtags" {
        return contains_hashset_string_type(ty);
    }
    if matches!(
        (parent.as_str(), field.as_str()),
        ("rootsystemcfg", "root_replaceable")
            | ("largedripstonecfg", "replaceable_blocks")
            | ("speleothemcfg", "replaceable_blocks")
            | ("speleothemclustercfg", "replaceable_blocks")
            | ("springcfg", "valid_blocks")
            | ("multifacegrowthcfg", "block" | "can_be_placed_on")
            | ("replaceblobscfg", "target")
            | ("hugefunguscfg", "valid_base_block")
            | ("vegetationpatchcfg", "replaceable")
    ) {
        return contains_state_text_type(ty);
    }
    false
}

fn contains_hashset_string_type(ty: &Type) -> bool {
    match ty {
        Type::Array(value) => contains_hashset_string_type(&value.elem),
        Type::Group(value) => contains_hashset_string_type(&value.elem),
        Type::Paren(value) => contains_hashset_string_type(&value.elem),
        Type::Reference(value) => contains_hashset_string_type(&value.elem),
        Type::Slice(value) => contains_hashset_string_type(&value.elem),
        Type::Tuple(value) => value.elems.iter().any(contains_hashset_string_type),
        Type::Path(value) => value.path.segments.iter().any(|segment| {
            if segment.ident == "HashSet" {
                return match &segment.arguments {
                    PathArguments::AngleBracketed(arguments) => arguments.args.iter().any(|argument| {
                        matches!(argument, syn::GenericArgument::Type(Type::Path(path))
                            if path.path.segments.last().is_some_and(|segment| segment.ident == "String"))
                    }),
                    _ => false,
                };
            }
            match &segment.arguments {
                PathArguments::AngleBracketed(arguments) => arguments.args.iter().any(|argument| {
                    if let syn::GenericArgument::Type(ty) = argument {
                        contains_hashset_string_type(ty)
                    } else {
                        false
                    }
                }),
                _ => false,
            }
        }),
        _ => false,
    }
}

fn text_conversion_name(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "encode_state_text" | "resident_tick_text")
}

fn state_conversion_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "name_of"
            | "canonical_state"
            | "to_text"
            | "state_name"
            | "block_state_name"
            | "format_state"
            | "render_state"
            | "as_state_text"
            | "to_state_string"
    )
}

fn call_name(expr: &Expr) -> String {
    match expr {
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn is_state_conversion_call(expr: &Expr, name: &str) -> bool {
    if !matches!(name, "from_state_str" | "canonical_state" | "name_of") {
        return false;
    }
    if name != "name_of" {
        return true;
    }
    !expr.to_token_stream().to_string().contains("enchantment_data")
}

fn boundary_symbol(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if matches!(name.as_str(), "biome_palette" | "biome_cell_palette" | "biome_state_at") {
        return true;
    }
    if matches!(
        name.as_str(),
        "biome_state"
            | "biome_for_carver_source"
            | "carvers_by_biome"
            | "packet_state_type"
            | "block_states_of"
            | "block_state_payload"
            | "held_item_overlay"
            | "trial_spawner_state_property"
            | "draw_beacon_button"
            | "poi_type_for_state"
    ) {
        return true;
    }
    name.ends_with("::setjigsawblock")
        || name == "setjigsawblock"
}

fn block_state_owner(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "blockstate" || name.ends_with("::blockstate") || name.contains("block_state")
}

fn contains_state_text_type(ty: &Type) -> bool {
    let structural = match ty {
        Type::Array(value) => contains_state_text_type(&value.elem),
        Type::Group(value) => contains_state_text_type(&value.elem),
        Type::Paren(value) => contains_state_text_type(&value.elem),
        Type::Reference(value) => contains_state_text_type(&value.elem),
        Type::Slice(value) => contains_state_text_type(&value.elem),
        Type::Tuple(value) => value.elems.iter().any(contains_state_text_type),
        Type::Path(value) => value.path.segments.iter().any(|segment| {
            if matches!(segment.ident.to_string().as_str(), "String" | "str") {
                return true;
            }
            match &segment.arguments {
                PathArguments::AngleBracketed(arguments) => arguments.args.iter().any(|argument| {
                    if let syn::GenericArgument::Type(ty) = argument {
                        contains_state_text_type(ty)
                    } else {
                        false
                    }
                }),
                PathArguments::Parenthesized(arguments) => {
                    arguments
                        .inputs
                        .iter()
                        .any(|argument| contains_state_text_type(&argument.ty))
                        || match &arguments.output {
                            syn::ReturnType::Type(_, ty) => contains_state_text_type(ty),
                            syn::ReturnType::Default => false,
                        }
                }
                PathArguments::None => false,
            }
        }),
        _ => false,
    };
    structural
        || ty.to_token_stream().to_string().split_whitespace().any(|part| {
            matches!(
                part.trim_matches(|c: char| !c.is_ascii_alphanumeric()),
                "String" | "str"
            )
        })
}

fn allowed_path(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    text.contains("/tests/")
        || text.contains("/test/")
        || text.contains("/fixtures/")
        || text.ends_with("/config.rs")
        || text.contains("/nbt/")
        || text.ends_with("/block_entities.rs")
        || text.ends_with("/chunk_nbt.rs")
        || text.contains("/packets/")
        || text.ends_with("/serialization.rs")
        || text.contains("/serialization/")
        || text.ends_with("/serialize.rs")
        || (text.ends_with("/action.rs") && text.contains("/lodestone-model/"))
}

fn should_scan(path: &Path) -> bool {
    if path.extension().and_then(|value| value.to_str()) != Some("rs")
        || (allowed_path(path) && !is_vegetation_config(path))
    {
        return false;
    }
    let text = path.to_string_lossy();
    text.contains("lodestone-worldgen/src")
        || text.contains("lodestone-client/src")
        || text.contains("lodestone-model/src")
        || text.contains("lodestone-server/src")
        || text.contains("lodestone-shell/src")
}

fn is_vegetation_config(path: &Path) -> bool {
    path.to_string_lossy().replace('\\', "/")
        == "crates/lodestone-worldgen/src/feature/vegetation/config.rs"
}

fn collect_paths(root: &Path, dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_paths(root, &path, paths)?;
        } else if should_scan(&path) {
            paths.push(path.strip_prefix(root).unwrap_or(&path).to_owned());
        }
    }
    Ok(())
}

fn scan_paths(root: &Path, paths: &[PathBuf]) -> Result<Report> {
    let mut violations = Vec::new();
    for relative in paths {
        if allowed_path(relative) && !is_vegetation_config(relative) {
            continue;
        }
        let path = root.join(relative);
        let source = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse {}", path.display()))?;
        if has_cfg_test(&syntax.attrs) {
            continue;
        }
        let mut visitor = Visitor {
            file: relative.display().to_string(),
            ..Visitor::default()
        };
        visitor.visit_file(&syntax);
        violations.extend(visitor.violations);
    }
    Ok(Report {
        files_scanned: paths.len(),
        violations,
    })
}

pub fn check_worldgen_state_ids(root: &Path) -> Result<Report> {
    let mut paths = Vec::new();
    for relative in ROOTS {
        collect_paths(root, &root.join(relative), &mut paths)?;
    }
    paths.sort();
    if paths.is_empty() {
        bail!("worldgen state-id guard scanned no production files");
    }
    scan_paths(root, &paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path, relative: &str, source: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).unwrap();
        fs::write(path, source).unwrap();
    }

    #[test]
    fn catches_state_string_and_text_conversion() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/overworld/decorate.rs",
            "struct Spill { state: String } struct BlockState { name: String } fn place(&self, state: LocalId) { self.interner.name_of(state); }",
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from("crates/lodestone-worldgen/src/overworld/decorate.rs")],
        )
        .unwrap();
        assert_eq!(report.violations.len(), 3, "{}", report.render());
    }

    #[test]
    fn allows_resource_strings_and_test_items() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/feature/config.rs",
            "struct Resource { block: String } fn parse(&self, state: String) { self.interner.name_of(state); }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/feature/fixture.rs",
            "#[cfg(test)] struct Spill { state: String }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-server/src/support_collapse_gate.rs",
            "#![cfg(test)] struct Spill { state: String }",
        );
        let report = scan_paths(
            tmp.path(),
            &[
                PathBuf::from("crates/lodestone-worldgen/src/feature/config.rs"),
                PathBuf::from("crates/lodestone-worldgen/src/feature/fixture.rs"),
                PathBuf::from("crates/lodestone-server/src/support_collapse_gate.rs"),
            ],
        )
        .unwrap();
        assert!(report.violations.is_empty(), "{}", report.render());
    }

    #[test]
    fn catches_tick_state_text_helper_even_when_named_encode() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-server/src/tick.rs",
            "fn encode_state_text(state: StateId) -> String { state.canonical_state() }",
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from("crates/lodestone-server/src/tick.rs")],
        )
        .unwrap();
        assert_eq!(report.violations.len(), 2, "{}", report.render());
    }

    #[test]
    fn catches_renamed_state_text_helper() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-server/src/hand_use.rs",
            "fn make_value(state: StateId) -> String { state.canonical_state() }",
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from("crates/lodestone-server/src/hand_use.rs")],
        )
        .unwrap();
        assert_eq!(report.violations.len(), 1, "{}", report.render());
    }

    #[test]
    fn catches_state_event_fields_and_generic_state_text_signatures() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-server/src/block_tick_reaction.rs",
            "struct RandomTickReaction { event: String, from: String, to: String, grid_row: Vec<String> } fn block_at() -> String { String::new() } fn state_name(value: String) -> String { value }",
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from("crates/lodestone-server/src/block_tick_reaction.rs")],
        )
        .unwrap();
        assert_eq!(report.violations.len(), 7, "{}", report.render());

        fixture(
            tmp.path(),
            "crates/lodestone-server/src/fluid.rs",
            "fn state_name_ref(value: &str) {}",
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from("crates/lodestone-server/src/fluid.rs")],
        )
        .unwrap();
        assert_eq!(report.violations.len(), 1, "{}", report.render());
    }

    #[test]
    fn allows_biome_carver_keys_and_json_state_ingress_only_at_its_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/nether/mod.rs",
            "struct Generator { carvers_by_biome: HashMap<String, Vec<CarverConfig>> } fn state_id_from_settings(value: Value) -> StateId { StateId::from_state_str(\"minecraft:air\") }",
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from("crates/lodestone-worldgen/src/nether/mod.rs")],
        )
        .unwrap();
        assert!(report.violations.is_empty(), "{}", report.render());

        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/overworld/mod.rs",
            "fn state_id_from_settings(value: Value) -> StateId { StateId::from_state_str(\"minecraft:air\") }",
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from("crates/lodestone-worldgen/src/overworld/mod.rs")],
        )
        .unwrap();
        assert_eq!(report.violations.len(), 1, "{}", report.render());

        fixture(
            tmp.path(),
            "crates/lodestone-server/src/properties.rs",
            "struct RawProperties { entries: Vec<(String, String)> } impl RawProperties { fn to_text(&self) -> String { String::new() } } impl ServerProperties { fn save(&self) { self.to_raw().to_text(); } }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-server/src/heavy_scene.rs",
            "fn setblock_state_id(command: &str) -> Option<StateId> { StateId::from_state_str(command) }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-server/src/world_storage.rs",
            "enum ChunkRecordError { InvalidPackedStates(String) }",
        );
        let report = scan_paths(
            tmp.path(),
            &[
                PathBuf::from("crates/lodestone-server/src/properties.rs"),
                PathBuf::from("crates/lodestone-server/src/heavy_scene.rs"),
                PathBuf::from("crates/lodestone-server/src/world_storage.rs"),
            ],
        )
        .unwrap();
        assert!(report.violations.is_empty(), "{}", report.render());
    }

    #[test]
    fn catches_block_set_storage_and_fallback_membership_api() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/feature/vegetation/config.rs",
            "use std::collections::HashSet; struct VegTags { logs: HashSet<String> } struct RootSystemCfg { root_replaceable: HashSet<String> } struct Resource { biomes: HashSet<String> } fn resolve_block_set() -> Option<HashSet<String>> { None }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/feature/vegetation/ids.rs",
            "struct Tags; impl Tags { fn member(&self, base: &str) -> bool { !base.is_empty() } }",
        );
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/feature/vegetation/features.rs",
            "use std::collections::HashSet; struct SpringCfg { valid_blocks: HashSet<String> } struct Resource { biomes: HashSet<String> }",
        );
        let report = scan_paths(
            tmp.path(),
            &[
                PathBuf::from("crates/lodestone-worldgen/src/feature/vegetation/config.rs"),
                PathBuf::from("crates/lodestone-worldgen/src/feature/vegetation/ids.rs"),
                PathBuf::from("crates/lodestone-worldgen/src/feature/vegetation/features.rs"),
            ],
        )
        .unwrap();
        assert_eq!(report.violations.len(), 5, "{}", report.render());
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.owner == "member")
        );
    }

    #[test]
    fn catches_state_text_entry_points_and_block_constants_without_registry_false_positives() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(
            tmp.path(),
            "crates/lodestone-worldgen/src/structure/runtime.rs",
            r#"
                const STONE: &str = "minecraft:stone";
                const DEFAULT_BIOME: &str = "minecraft:plains";
                const PLAYER_ENTITY_TYPE: &str = "minecraft:player";
                const LOOT_TABLE: &str = "minecraft:chests/simple_dungeon";
                fn state_id(spec: &str) -> StateId { StateId::from_state_str(spec) }
                fn base_name(state: &str) -> &str { state }
                fn item_id(value: &str) -> &str { value }
                fn place() -> StateId { state_id("minecraft:stone") }
            "#,
        );
        let report = scan_paths(
            tmp.path(),
            &[PathBuf::from(
                "crates/lodestone-worldgen/src/structure/runtime.rs",
            )],
        )
        .unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.owner == "STONE"),
            "{}",
            report.render()
        );
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.owner == "state_id"),
            "{}",
            report.render()
        );
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.owner == "base_name"),
            "{}",
            report.render()
        );
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.reason.contains("state_id()")),
            "{}",
            report.render()
        );
        assert!(
            report
                .violations
                .iter()
                .all(|violation| !matches!(
                    violation.owner.as_str(),
                    "DEFAULT_BIOME" | "PLAYER_ENTITY_TYPE" | "LOOT_TABLE" | "item_id"
                )),
            "{}",
            report.render()
        );
    }
}
