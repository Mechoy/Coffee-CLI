# Mechoy Distribution

This repository is a customized Coffee CLI distribution. It keeps upstream
changes while publishing its own update channel so a normal Coffee CLI update
cannot replace local multi-agent, recovery, or MCP functionality.

## Identity

- Application version: plain `x.y.z` in `Cargo.toml` and `tauri.conf.json`.
- Visible product: `Coffee CLI Mechoy` / `Mechoy Build`.
- Bundle identifier: `com.mechoy.coffeecli`.
- Release tag: `mechoy-v<x.y.z>`.
- Release repository: `Mechoy/Coffee-CLI`.
- Published assets: `Coffee.CLI_Mechoy_<version>_<platform>_<arch>.<ext>`.

The plain version is intentional. Tauri, native installers, and the frontend
version comparator expect three numeric components. The Mechoy identity lives
in the product name and release tag namespace instead of a SemVer suffix.

## Upstream Sync

1. Commit or otherwise preserve local work first.
2. Fetch `upstream` and inspect its release/tag changes.
3. Merge the desired upstream release into `main`; resolve shared files by
   preserving both upstream behavior and Mechoy functionality.
4. Run `cargo check`, `cargo test`, and `cd src-ui && npm run build`.
5. Bump the local patch version only when preparing a new Mechoy package.

Never force-push or replace `main` with upstream. The local feature commits and
the upstream sync merge must both remain in this repository's history.

## Release

1. Set the same next `x.y.z` in `Cargo.toml` and `tauri.conf.json`.
2. Run the Rust and frontend checks above.
3. Commit the release preparation with `chore(release): v<x.y.z>`.
4. Create `mechoy-v<x.y.z>` on that commit.
5. Push `main` and that `mechoy-v*` tag. The Release workflow builds a draft
   release; publish it after checking the uploaded assets.

Do not use a `v<x.y.z>` tag for a Mechoy release. It belongs to the upstream
namespace and is intentionally ignored by the Mechoy release workflow.

## Install And Update Paths

The application and install scripts read `Web-Home/mechoy-version.json`, which
CI updates only after a Mechoy release is published. They then construct the
exact tag and asset URL, avoiding unauthenticated GitHub Releases API limits. A
deployed copy of `Web-Home` may use its Worker to proxy the same assets, but
the upstream `coffeecli.com` site is not a Mechoy distribution endpoint.

The custom and official desktop applications use different native identities.
They deliberately continue to share `~/.coffee-cli` data so existing sessions,
hooks, and MCP configuration remain available. On Linux, native packages still
both provide the `coffee-cli` command for compatibility, so the installer asks
the user to remove an old `coffee-cli` package before installing the Mechoy
package. The data directory is not removed by that package change.
