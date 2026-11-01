# Okytes Rust SDK

`okytes-sdk` provides two explicit models:

- `EmbeddedEngine` owns an isolated state database and transfer workers.
- `DaemonClient` controls an existing authenticated Okytes daemon and never opens its database.

See `dev-tools/sdk/docs/rust.md` for setup, ownership, recovery, and examples.

## Plugins (preview, 0.0.2)

`okytes_sdk::plugin` loads and validates an `okytes-plugin.json` manifest and
connects with the plugin's own permission-restricted token:

```rust
use okytes_sdk::plugin::{Manifest, Permission, Plugin};

let plugin = Plugin::from_environment(Manifest::load(std::path::Path::new("."))?)?;
plugin.require(Permission::JobsRead)?;
```

See the [plugin guide](../../plugins/README.md).

