# Friends service

## What it is

`lodestone-auth::friends` is the credential-safe HTTP boundary for the Java 26.2 Friends List. It turns an already-resolved account session into typed friend lists, relationship changes, service preferences, and presence without exposing the bearer token to menu or rendering code.

## How it works

`FriendsService::production` owns a five-second, redirect-disabled HTTP client and a fixed `https://api.minecraftservices.com/` origin. A caller passes `&Session` to each operation, so the service neither opens token storage nor refreshes credentials. The session's bearer header is added only to the fixed request origin.

The native constructor is the only enabled production path today. On `wasm32`, the constructor returns `Unavailable`: the browser transport exposed here cannot enforce the no-redirect policy or bounded incremental response reads. Keep that fail-closed behavior until a browser-specific service path has been verified.

The implementation uses the documented route family: `GET`/`PUT /friends`, `GET`/`POST /player/attributes`, and `POST /presence`. The 26.2 shipped service-library artifact in the local reference cache was inspected to verify those methods, paths, request names, response fields, cache validators, and presence values. Official 26.2 release notes independently confirm the user-facing Friends behavior, polling cadence, request controls, and presence controls. [Release notes](https://feedback.minecraft.net/hc/en-us/articles/46690753273997-Minecraft-Java-Edition-26-2)

`get_friends` and `publish_presence` preserve `ETag`/`If-None-Match` conditional behavior through `CachedResponse`; `304 Not Modified` never becomes an empty snapshot. A declared length above 1 MiB is rejected early, while chunked responses are accumulated incrementally and rejected immediately once they exceed that cap. Unknown presence values affect only their row. Peer-messaging identity and join metadata are intentionally not represented, persisted, or forwarded.

The non-default `friends-test-service` feature exposes `for_test_base` for downstream hermetic tests. It accepts only `http` or `https` loopback origins. Production has no origin override, including through environment variables or preferences.

## How to change it

Keep wire request and response structs private in `lodestone_auth::friends`; promote only domain values that a runtime or menu genuinely consumes. Add a loopback test that asserts the exact method, path, bearer header, validators, and JSON body before changing a route or field. Do not add invitation, join, signaling, or peer identity fields here: that transport was removed before 26.2 release, as the official pre-release notes record. [Pre-release boundary](https://feedback.minecraft.net/hc/en-us/articles/46153634280333-Minecraft-Java-Edition-26-2-Pre-release-1)

If a new HTTP client is needed, construct it inside this module with redirects disabled. Accepting an arbitrary caller-built client can silently re-enable token forwarding on redirects. Never log response bodies, submitted names, or `Session` values.

## Configuration

- Production origin: fixed `https://api.minecraftservices.com/`.
- Request timeout: five seconds.
- Maximum successful JSON body: 1 MiB; `Content-Length` is an early check, not a requirement.
- Test origin override: `friends-test-service` only, and loopback-only.
- Browser target: disabled fail-closed until its transport can enforce the same redirect and response-size rules.

## Dependencies

The module depends on `reqwest` for HTTPS and headers, `serde`/`serde_json` for private wire shapes, `uuid` for profile identifiers, and the existing `lodestone-auth::Session` resolver boundary. It does not depend on the shell, a protocol family, persistent Friends state, or any multiplayer invitation transport.
