use super::*;
use crate::dialect::FixedRegistryKind;
use lodestone_data::version::GameDataVersion;

/// The release and synchronized registry view used by every nested item value.
#[derive(Clone, Copy)]
pub(crate) struct StackCodecContext<'a> {
    pub(crate) dialect: ProtocolDialect,
    registries: Option<&'a ClientRegistries>,
}

impl<'a> StackCodecContext<'a> {
    pub(crate) fn new(dialect: ProtocolDialect, registries: &'a ClientRegistries) -> Self {
        Self { dialect, registries: Some(registries) }
    }

    pub(crate) fn v26_2() -> Self {
        Self { dialect: ProtocolDialect::v26_2(), registries: None }
    }

    pub(crate) fn latest(self) -> bool {
        self.dialect.game_data_version() == GameDataVersion::V26_3
    }

    pub(crate) fn has_synchronized_registries(self) -> bool {
        self.registries.is_some()
    }

    pub(crate) fn item(self, raw: i32) -> Result<Item, AdapterError> {
        u32::try_from(raw).ok()
            .and_then(|id| self.dialect.game_data_version().item_from_wire(id))
            .ok_or_else(|| AdapterError::Decode(format!("unknown item registry id {raw}")))
    }

    pub(crate) fn recipe_item(self, raw: i32) -> Result<ItemId, AdapterError> {
        let id = u32::try_from(raw)
            .map_err(|_| AdapterError::Decode("negative item registry id".to_owned()))?;
        Ok(match self.dialect.game_data_version().item_from_wire(id) {
            Some(item) => ItemId::canonical(u32::from(item.registry_id())),
            None => ItemId::protocol_local(id),
        })
    }

    pub(crate) fn fixed(self, kind: FixedRegistryKind, raw: i32) -> Result<i32, AdapterError> {
        self.dialect.canonical_fixed_id(kind, raw)
    }

    pub(crate) fn component(self, raw: i32) -> Result<Option<&'static str>, AdapterError> {
        let canonical = self.fixed(FixedRegistryKind::DataComponent, raw)?;
        Ok(DataComponentTypeId::new(canonical).map(component_type_name))
    }

    pub(crate) fn dynamic(self, registry: &str, raw: i32) -> Result<String, AdapterError> {
        let index = usize::try_from(raw)
            .map_err(|_| AdapterError::Decode(format!("negative {registry} holder {raw}")))?;
        self.registries.and_then(|registries| registries.entry_names(registry))
            .and_then(|entries| entries.get(index)).cloned()
            .ok_or_else(|| AdapterError::Unsupported(format!(
                "{registry} holder {raw} has no synchronized registry entry"
            )))
    }

    pub(crate) fn dynamic_holder(self, registry: &str, holder: i32) -> Result<String, AdapterError> {
        let raw = holder.checked_sub(1).filter(|_| holder > 0)
            .ok_or_else(|| AdapterError::Decode(format!("invalid {registry} holder {holder}")))?;
        self.dynamic(registry, raw)
    }

    pub(crate) fn fixed_holder(self, kind: FixedRegistryKind, holder: i32) -> Result<i32, AdapterError> {
        let raw = holder.checked_sub(1).filter(|_| holder > 0)
            .ok_or_else(|| AdapterError::Decode(format!("invalid {kind:?} holder {holder}")))?;
        self.fixed(kind, raw)
    }
}
