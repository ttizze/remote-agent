Lucide 0.468.0, ISC license, vendored for offline rendering.
Source: https://registry.npmjs.org/lucide/-/lucide-0.468.0.tgz
Archive SHA256: cab2924552d273da12de0beab51d81ef2e2d397b0ac0eef010eaa02fd4ae5764
The complete upstream UMD bundle supplies the same icon API on both mobile platforms.

Reproduce these vendored files with the pinned npm lockfile:

```sh
nix develop . --command npm --prefix crates/host-daemon/src/visualize ci --ignore-scripts --no-audit
nix develop . --command npm --prefix crates/host-daemon/src/visualize run vendor
```
