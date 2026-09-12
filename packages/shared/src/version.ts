/**
 * The one version string the runtime reports.
 *
 * This is deliberately a constant rather than a read of the nearest
 * `package.json`: the sidecar runs from a built `dist/` bundle and the UI runs from
 * a bundled asset graph, so a file lookup would work in dev and silently fall back
 * in a packaged build. A version report has to be the same value in every mode, so
 * it is a literal here.
 *
 * "Every manifest agrees with this constant" is not a convention, it is a gate:
 * `version.test.ts` reads `package.json`, `Cargo.toml`, `tauri.conf.json` and the
 * contract fixtures and fails when any of them disagrees. Bumping a release means
 * editing this file, the manifests it names, and `CHANGELOG.md` — the test says so
 * instead of a reviewer having to remember.
 */
export const APP_VERSION = "0.1.0";
