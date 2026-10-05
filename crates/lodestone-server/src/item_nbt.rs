//! One item stack in its save-file form: `{id, count, components?}`.
//!
//! Every place that saves a stack — container block entities, the Anvil
//! player file, the native player record — goes through this module, so a
//! component the server can put on a stack survives a restart wherever the
//! stack sits. The component shapes are the reference server's own; the
//! witness is `crates/lodestone-server/tests/item_nbt_vanilla.rs`, which
//! reads a chest the official server saved and writes it back.
//!
//! A stack carrying something this form does not cover (an armour trim, a
//! profile, an inline instrument, a component the wire decoder could not
//! model, …) is still written with everything that *is* covered, and
//! [`Persisted::complete`] says so. Callers that must not lose data — the
//! player records — refuse such a stack; a container keeps what it can.

use lodestone_core::{Nbt, NbtTag, Reader, Writer, read_network_nbt, write_network_nbt};
use lodestone_model::{
    ItemComponents, ItemEnchantment, ItemInstrument, ItemStack, ResourceKey, Text,
    WrittenBookContent,
};

/// A stack's save-file compound and whether it carries everything.
#[derive(Debug, Clone, PartialEq)]
pub struct Persisted {
    /// `{id, count, components?}`; the caller adds its own fields (`Slot`).
    pub fields: Vec<(String, Nbt)>,
    /// `false` when some component on the stack has no saved form here and
    /// was left out.
    pub complete: bool,
}

/// The save-file form of `stack`.
#[must_use]
pub fn stack_to_nbt(stack: &ItemStack) -> Persisted {
    let (components, complete) = components_to_nbt(&stack.item, &stack.components);
    let mut fields = vec![
        ("id".to_owned(), Nbt::String(stack.item.to_string())),
        ("count".to_owned(), Nbt::Int(i32::try_from(stack.count).unwrap_or(i32::MAX))),
    ];
    if !components.is_empty() {
        fields.push(("components".to_owned(), Nbt::Compound(components)));
    }
    Persisted { fields, complete }
}

/// Reads a saved stack, or `None` when it has no usable `id`. A component
/// this module does not read marks the stack
/// [`has_unmodeled`](ItemComponents::has_unmodeled), so a later strict save
/// refuses it instead of writing it back without that component.
#[must_use]
pub fn stack_from_nbt(nbt: &Nbt) -> Option<ItemStack> {
    let Some(Nbt::String(id)) = field(nbt, "id") else { return None };
    let key: ResourceKey = id.parse().ok()?;
    // `count` is an Int in the current form; older files wrote a Byte.
    let count = match field(nbt, "count").or_else(|| field(nbt, "Count")) {
        Some(Nbt::Int(c)) => (*c).max(0) as u32,
        Some(Nbt::Byte(c)) => i32::from(*c).max(0) as u32,
        _ => 1,
    };
    let mut stack = ItemStack::new(key, count);
    match field(nbt, "components") {
        None => {}
        Some(Nbt::Compound(components)) => {
            for (name, value) in components {
                if !read_component(&mut stack.components, name, value) {
                    stack.components.has_unmodeled = true;
                }
            }
        }
        Some(_) => stack.components.has_unmodeled = true,
    }
    Some(stack)
}

/// A stack's saved `components` compound as network NBT, the form the native
/// store keeps in one bytes field (empty when the stack has none), and
/// whether it is [complete](Persisted::complete).
#[must_use]
pub fn components_to_bytes(stack: &ItemStack) -> (Vec<u8>, bool) {
    let persisted = stack_to_nbt(stack);
    let Some((_, compound)) = persisted.fields.into_iter().find(|(name, _)| name == "components") else {
        return (Vec::new(), persisted.complete);
    };
    let mut writer = Writer::default();
    match write_network_nbt(&mut writer, &compound) {
        Ok(()) => (writer.into_vec(), persisted.complete),
        Err(_) => (Vec::new(), false),
    }
}

/// Reads [`components_to_bytes`]' form back onto `stack`. `false` when the
/// bytes are malformed or carry a component this module does not read.
#[must_use]
pub fn apply_components_bytes(stack: &mut ItemStack, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    let mut reader = Reader::new(bytes);
    let Ok(compound) = read_network_nbt(&mut reader) else { return false };
    if reader.ensure_empty().is_err() {
        return false;
    }
    let saved = Nbt::Compound(vec![
        ("id".to_owned(), Nbt::String(stack.item.to_string())),
        ("components".to_owned(), compound),
    ]);
    match stack_from_nbt(&saved) {
        Some(read) if !read.components.has_unmodeled => {
            let custom_data = stack.components.custom_data.take();
            stack.components = read.components;
            if stack.components.custom_data.is_none() {
                stack.components.custom_data = custom_data;
            }
            true
        }
        _ => false,
    }
}

/// The compound root of a custom-data component as save-file NBT, or `None`
/// when the model bytes are not one complete compound-root network value.
#[must_use]
pub fn custom_data_to_nbt(bytes: &[u8]) -> Option<Nbt> {
    let mut reader = Reader::new(bytes);
    let value = read_network_nbt(&mut reader).ok()?;
    reader.ensure_empty().ok()?;
    matches!(value, Nbt::Compound(_)).then_some(value)
}

/// The model bytes of a saved custom-data compound.
#[must_use]
pub fn custom_data_from_nbt(value: &Nbt) -> Option<Vec<u8>> {
    let Nbt::Compound(_) = value else { return None };
    let mut writer = Writer::default();
    write_network_nbt(&mut writer, value).ok()?;
    Some(writer.into_vec())
}

fn components_to_nbt(item: &ResourceKey, components: &ItemComponents) -> (Vec<(String, Nbt)>, bool) {
    let mut out: Vec<(String, Nbt)> = Vec::new();
    let mut put = |name: &str, value: Nbt| out.push((format!("minecraft:{name}"), value));
    // Everything not written below must be at its default for the stack to be
    // complete; the written fields are cleared from this copy as they go.
    let mut rest = components.clone();
    let mut complete = !components.has_unmodeled;

    if let Some(bytes) = rest.custom_data.take() {
        match custom_data_to_nbt(&bytes) {
            Some(value) => put("custom_data", value),
            None => complete = false,
        }
    }
    if let Some(damage) = rest.damage.take() {
        match i32::try_from(damage) {
            Ok(damage) => put("damage", Nbt::Int(damage)),
            Err(_) => complete = false,
        }
    }
    let enchantments = std::mem::take(&mut rest.enchantments);
    if !enchantments.is_empty() {
        let mut levels = Vec::with_capacity(enchantments.len());
        for enchantment in &enchantments {
            match (crate::enchantment_data::name_of(enchantment.id), i32::try_from(enchantment.level)) {
                (Some(name), Ok(level)) => levels.push((name.to_owned(), Nbt::Int(level))),
                _ => complete = false,
            }
        }
        // A book's list is what it can apply, saved under its own name.
        let name = if item.to_string() == "minecraft:enchanted_book" { "stored_enchantments" } else { "enchantments" };
        put(name, Nbt::Compound(levels));
    }
    if let Some(name) = rest.custom_name.take() {
        put("custom_name", name.to_nbt());
    }
    let lore = std::mem::take(&mut rest.lore);
    if !lore.is_empty() {
        put("lore", text_list(&lore));
    }
    if let Some(color) = rest.dyed_color.take() {
        put("dyed_color", Nbt::Int(color as i32));
    }
    if rest.repair_cost > 0 {
        match i32::try_from(rest.repair_cost) {
            Ok(cost) => put("repair_cost", Nbt::Int(cost)),
            Err(_) => complete = false,
        }
        rest.repair_cost = 0;
    }
    let potion = rest.potion.take();
    let potion_name = rest.potion_custom_name.take();
    if potion.is_some() || potion_name.is_some() {
        let mut fields = Vec::new();
        if let Some(potion) = potion {
            match lodestone_data::potion::PotionId::from_registry_id(potion) {
                Some(id) => fields.push(("potion".to_owned(), Nbt::String(lodestone_data::potion::potion_name(id).to_owned()))),
                None => complete = false,
            }
        }
        if let Some(name) = potion_name {
            fields.push(("custom_name".to_owned(), Nbt::String(name)));
        }
        put("potion_contents", Nbt::Compound(fields));
    }
    // Recomputed from the potion on the way back in.
    rest.potion_color = None;
    if let Some(ItemInstrument::Reference(key)) = &rest.instrument {
        put("instrument", Nbt::String(key.to_string()));
        rest.instrument = None;
    }
    if let Some(pages) = rest.writable_book_content.take() {
        let pages = pages.into_iter().map(|page| Nbt::Compound(vec![("raw".to_owned(), Nbt::String(page))])).collect();
        put("writable_book_content", Nbt::Compound(vec![("pages".to_owned(), compound_list(pages))]));
    }
    if let Some(book) = rest.written_book_content.take() {
        put("written_book_content", written_book_to_nbt(&book));
    }
    // Prototype-derived fields: the item's own defaults, restored from the
    // prototype census, never saved.
    rest.max_stack_size = None;
    rest.max_damage = None;
    rest.equippable = None;
    rest.wire_patch_nonempty = false;
    rest.has_unmodeled = false;
    complete &= rest == ItemComponents::default();
    (out, complete)
}

fn read_component(components: &mut ItemComponents, name: &str, value: &Nbt) -> bool {
    let Some(name) = name.strip_prefix("minecraft:") else { return false };
    match (name, value) {
        ("custom_data", value) => custom_data_from_nbt(value).map(|bytes| components.custom_data = Some(bytes)).is_some(),
        ("damage", Nbt::Int(damage)) if *damage >= 0 => {
            components.damage = Some(*damage as u32);
            true
        }
        ("repair_cost", Nbt::Int(cost)) if *cost >= 0 => {
            components.repair_cost = *cost as u32;
            true
        }
        ("enchantments" | "stored_enchantments", Nbt::Compound(levels)) => {
            let mut all_known = true;
            for (key, level) in levels {
                match (crate::enchantment_data::id_of(key), level) {
                    (Some(id), Nbt::Int(level)) if *level > 0 => {
                        components.enchantments.push(ItemEnchantment { id, level: *level as u32 });
                    }
                    _ => all_known = false,
                }
            }
            all_known
        }
        ("custom_name", value) => {
            components.custom_name = Some(Text::from_nbt(value));
            true
        }
        ("lore", Nbt::List { elements, .. }) => {
            components.lore = elements.iter().map(Text::from_nbt).collect();
            true
        }
        ("dyed_color", Nbt::Int(color)) => {
            components.dyed_color = Some(*color as u32);
            true
        }
        ("potion_contents", value) => read_potion_contents(components, value),
        ("instrument", Nbt::String(key)) => key.parse().map(|key| components.instrument = Some(ItemInstrument::Reference(key))).is_ok(),
        ("writable_book_content", value) => {
            let pages = match field(value, "pages") {
                None => Some(Vec::new()),
                Some(Nbt::List { elements, .. }) => elements.iter().map(filterable_string).collect(),
                Some(_) => None,
            };
            pages.map(|pages| components.writable_book_content = Some(pages)).is_some()
        }
        ("written_book_content", value) => written_book_from_nbt(value).map(|book| components.written_book_content = Some(book)).is_some(),
        _ => false,
    }
}

/// `potion_contents` is a compound, or the bare potion id in its short form.
fn read_potion_contents(components: &mut ItemComponents, value: &Nbt) -> bool {
    let (potion, name, complete) = match value {
        Nbt::String(potion) => (Some(potion.as_str()), None, true),
        Nbt::Compound(fields) => {
            let potion = match field(value, "potion") {
                Some(Nbt::String(potion)) => Some(potion.as_str()),
                _ => None,
            };
            let name = match field(value, "custom_name") {
                Some(Nbt::String(name)) => Some(name.clone()),
                _ => None,
            };
            let known = fields.iter().all(|(key, _)| key == "potion" || key == "custom_name");
            (potion, name, known)
        }
        _ => return false,
    };
    let id = match potion {
        Some(potion) => match lodestone_data::potion::PotionId::from_name(potion) {
            Some(id) => Some(id),
            None => return false,
        },
        None => None,
    };
    components.potion = id.map(lodestone_data::potion::PotionId::registry_id);
    components.potion_color = Some(lodestone_data::potion::potion_color(id, None, &[]));
    components.potion_custom_name = name;
    complete
}

fn written_book_to_nbt(book: &WrittenBookContent) -> Nbt {
    let pages: Vec<Nbt> = book
        .pages
        .iter()
        .map(|page| Nbt::Compound(vec![("raw".to_owned(), page.to_nbt())]))
        .collect();
    let mut fields = vec![
        ("title".to_owned(), Nbt::Compound(vec![("raw".to_owned(), Nbt::String(book.title.clone()))])),
        ("author".to_owned(), Nbt::String(book.author.clone())),
    ];
    if book.generation != 0 {
        fields.push(("generation".to_owned(), Nbt::Int(i32::from(book.generation))));
    }
    if !pages.is_empty() {
        fields.push(("pages".to_owned(), compound_list(pages)));
    }
    if book.resolved {
        fields.push(("resolved".to_owned(), Nbt::Byte(1)));
    }
    Nbt::Compound(fields)
}

fn written_book_from_nbt(value: &Nbt) -> Option<WrittenBookContent> {
    let title = filterable_string(field(value, "title")?)?;
    let Some(Nbt::String(author)) = field(value, "author") else { return None };
    let generation = match field(value, "generation") {
        None => 0,
        Some(Nbt::Int(generation)) => u8::try_from(*generation).ok()?,
        Some(_) => return None,
    };
    let pages = match field(value, "pages") {
        None => Vec::new(),
        Some(Nbt::List { elements, .. }) => elements
            .iter()
            .map(|page| match page {
                Nbt::Compound(_) if field(page, "raw").is_some() => field(page, "raw").map(Text::from_nbt),
                other => Some(Text::from_nbt(other)),
            })
            .collect::<Option<Vec<_>>>()?,
        Some(_) => return None,
    };
    let resolved = matches!(field(value, "resolved"), Some(Nbt::Byte(1)));
    Some(WrittenBookContent { title, author: author.clone(), generation, pages, resolved })
}

/// A filterable string: `{raw, filtered?}`, or the bare string.
fn filterable_string(value: &Nbt) -> Option<String> {
    match value {
        Nbt::String(raw) => Some(raw.clone()),
        Nbt::Compound(_) => match field(value, "raw") {
            Some(Nbt::String(raw)) => Some(raw.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Text components as one NBT list. A list's elements share a tag, so when
/// any element needs a compound, the bare-string ones become `{text: …}`.
fn text_list(texts: &[Text]) -> Nbt {
    let nbts: Vec<Nbt> = texts.iter().map(Text::to_nbt).collect();
    if nbts.iter().all(|nbt| matches!(nbt, Nbt::String(_))) {
        return Nbt::List { element_type: NbtTag::String, elements: nbts };
    }
    let elements = nbts
        .into_iter()
        .map(|nbt| match nbt {
            Nbt::String(text) => Nbt::Compound(vec![("text".to_owned(), Nbt::String(text))]),
            other => other,
        })
        .collect();
    compound_list(elements)
}

fn compound_list(elements: Vec<Nbt>) -> Nbt {
    Nbt::List { element_type: if elements.is_empty() { NbtTag::End } else { NbtTag::Compound }, elements }
}

fn field<'a>(compound: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match compound {
        Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}
