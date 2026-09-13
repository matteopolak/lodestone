# Auth JSON contracts

## What it is

`lodestone-auth` keeps the account roster and its authentication request bodies as typed Serde
documents. The types make persisted metadata tolerant at the read boundary while keeping every
outbound field name explicit at the wire boundary.

## How it works

`AccountsMetadata::from_json` first deserializes an `AccountsMetadataDocument`. Invalid top-level
JSON still produces an empty roster; an invalid `selected` value becomes `None`; a missing or
non-array `profiles` field becomes empty; and each malformed profile entry is skipped independently.
Optional `skin_url` and `last_used` fields retain their `None` and `0` defaults when they have the
wrong type. Saving serializes the same typed document, so the established `profiles.json` shape
and key ordering remain unchanged.

The Xbox Live, XSTS, Minecraft-services login, and session-server join bodies are private Serde
request structs. `serde(rename = ...)` records the casing required by each service, and `bon`
builders name repeated string inputs such as `access_token`, `selected_profile`, and `server_id`.
Golden tests serialize each request against hand-written wire JSON so a Rust field rename cannot
silently alter an external contract.

## How to change it

When adding a roster field, update both the typed document and its conversion to/from the public
metadata types. Decide explicitly whether a malformed value should skip one entry or default one
field, then add a negative test for that behavior. When adding an HTTP request field, add the
Serde rename and extend the corresponding golden serialization test; use a builder whenever a
constructor accepts multiple values of the same type.

Do not reintroduce `serde_json::Value` for these known schemas. Keep request structs private unless
another crate genuinely needs to construct the external document, and never log request bodies:
several carry live access or refresh tokens.

## Configuration

No additional configuration controls these schemas. The persisted roster path comes from
`lodestone-auth::paths::profiles_path`; HTTP endpoints and their fixed field names are constants in
`lodestone-auth::flow`.

## Dependencies

The documents use `serde` and `serde_json`; UUID fields use `uuid`; request builders use `bon`;
HTTP serialization is performed by `reqwest`. The metadata document is shared by the account
switcher and the login/migration code, while request structs are consumed only by the auth flow.
