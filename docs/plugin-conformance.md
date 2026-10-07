# Plugin author conformance kit

The conformance command validates a plugin project offline. It does not install
or enable the plugin in an operator database.

```powershell
cargo run -p execlaw-plugin-host --bin execlaw-plugin-conformance -- init `
  --path .\my-plugin --plugin-id my-plugin --tier script
cargo run -p execlaw-plugin-host --bin execlaw-plugin-conformance -- check .\my-plugin
```

Use `--tier subprocess` to generate the JSON-RPC process example. The generated
project includes input and result schemas plus fixture calls. `check` uses the
production manifest parser and schema loader, verifies fixture inputs and
outputs, checks that fixtures request only the tool's declared capabilities,
and executes each fixture through the declared script or subprocess runtime
with mock inputs. The subprocess receives the same curated OS environment as
installed plugins.

To compare an upgrade before publishing it:

```powershell
cargo run -p execlaw-plugin-host --bin execlaw-plugin-conformance -- upgrade `
  --previous .\release-1\plugin.toml --candidate .\release-2\plugin.toml
```

The upgrade check rejects non-increasing versions, added tool authority,
lowered trust floors, and changed effect contracts. Error messages identify the
tool and field that needs review. Schema, runtime, and lifecycle cases can be
extended in `tests/conformance.json` without a configured execlaw installation.

Generate the published machine-readable manifest reference from the same
`PluginManifest` structs used by parsing and installation:

```powershell
cargo run -p execlaw-plugin-host --bin execlaw-plugin-conformance -- schema `
  --output docs/plugin-manifest.schema.json
```

The checked-in generator tests exercise both runtime tiers; host integration
tests in `crates/server/tests/plugin_lifecycle.rs` additionally cover ZIP
install, enable, disable, uninstall, duplicate tools, invalid schemas, runtime
tiers, and upgrades through the public server route.

## Host API compatibility

`plugin.version` describes the plugin release. The optional `[compatibility]`
table in `plugin.toml` describes the host API contract:

```toml
[compatibility]
host_api = ">=1.0.0, <2.0.0"
required_features = ["jsonrpc.line.v1", "tool.schema.validation.v1"]

[[compatibility.deprecated_primitives]]
primitive = "host_log"
replacement = "log_info"
```

An unsupported range, unknown required feature, or unknown compatibility field
fails manifest validation before installation. Bundles that predate this table
remain readable through the legacy path. Deprecation diagnostics name the
primitive and replacement and do not alter its current authorization or effect
semantics.
