//! Load-time 26.3 resource decoding into the shared typed execution model.
//!
//! Resource strings are interpreted by their field's domain: a provider holder
//! string names a provider resource, while a state string names a block. No
//! document lookup or state-name parsing remains in a baked provider.

mod provider;
mod state;
mod material;

pub use provider::ProviderBaker;
pub use state::parse_state;
pub use material::{BakedMaterial, MaterialBaker, MaterialCondition, MaterialDensityId,
    MaterialGraph, MaterialInputs, MaterialNoiseId, MaterialRandomId, MaterialSampling};

/// A rejected resource path and the exact contract that could not be satisfied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontendError {
    pub path: String,
    pub reason: String,
}

impl FrontendError {
    pub(super) fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { path: path.into(), reason: reason.into() }
    }
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for FrontendError {}

pub(super) fn resource_id(value: &str, path: &str) -> Result<String, FrontendError> {
    let (namespace, name) = value.split_once(':').unwrap_or(("minecraft", value));
    let valid_namespace = !namespace.is_empty()
        && namespace.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c));
    let valid_name = !name.is_empty()
        && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_./-".contains(&c));
    if !valid_namespace || !valid_name {
        return Err(FrontendError::new(path, format!("invalid resource identifier {value:?}")));
    }
    Ok(format!("{namespace}:{name}"))
}
