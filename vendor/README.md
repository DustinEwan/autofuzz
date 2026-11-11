# Vendored packages

Sources here are copied from upstream repositories so `afz scan` has something real to
discovery-test against. Only what the scan reads is kept: `src/`, the package manifest, and
the upstream licence. Tests, build configuration, and `node_modules` are not copied, and no
package here is imported by the engine or the plugin.

| Package | Version | Upstream commit | Upstream | Licence |
| --- | --- | --- | --- | --- |
| `ufo` | 1.6.4 | `f06c800d0c59f2a4a1b9ba65eb6cb61a84419be6` | https://github.com/unjs/ufo | MIT |
| `defu` | 6.1.7 | `82632b66f5914e9946edce300e10633a3d5c0cb7` | https://github.com/unjs/defu | MIT |
| `destr` | 2.0.5 | `541b6f9aeada9fc30de9c5a7e086dbfc1c6fcdc7` | https://github.com/unjs/destr | MIT |
| `mitt` | 3.0.1 | `6b41670516ed8e8b738612f60491995470aa63b3` | https://github.com/developit/mitt | MIT |

Each package declares no runtime dependencies, which is what lets the scan materialize its
signatures without a workspace to install. `autofuzz.json` in each directory is the scan
project; the scan itself is recorded in `docs/dogfood/`.
