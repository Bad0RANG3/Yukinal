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
 * `version.test.ts` reads the JavaScript manifests and the version-carrying contract
 * fixtures, and fails when any of them disagrees. Bumping a release means editing this
 * file and the manifests that gate names; it does not know about any prose, so a release
 * note has to be updated by hand.
 */
export const APP_VERSION = "2.0.3";

/**
 * The Python Agent's version, which has its own release cadence.
 *
 * `agent/pyproject.toml` is the source of truth for the Python package, and CI builds
 * the sidecar from it. This constant exists because the desktop UI needs to *show*
 * that version in offline/preview mode, where no sidecar is running to ask — a
 * hand-written literal in the fixture is how the preview window ended up claiming
 * `1.0.0-preview` for a `0.1.0` sidecar. `version.test.ts` compares this literal with
 * `agent/pyproject.toml`, so the two cannot drift without a red gate.
 */
export const AGENT_VERSION = "0.1.0";
