pub const FAMILIES: &[Family] = &[
    #[cfg(feature = "v1")]
    Family { make: || Box::new(lodestone_v1::adapter()) },
];
