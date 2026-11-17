# Temporary picomatch security backport

This directory contains the npm `picomatch@4.0.7` runtime, with its original
package name, version and MIT license. It is a locally patched dependency, not
an official patched release. The workspace override applies it to Vite,
tinyglobby and fdir.

Source: [official npm tarball](https://registry.npmjs.org/picomatch/-/picomatch-4.0.7.tgz).
The downloaded archive was verified before extraction against this SHA-512 SRI:

```text
sha512-qcJu88Q2IWqJsDD529JKMdwGm/dvInW4HvQnRwiH9JtihJvzGOscDtHE3x1pBKeUOTysQ8kVmLnJ2kJu7yhcGA==
```

As of 2026-11-18, the npm registry has no 4.0.8 release. The
[GitHub advisory](https://github.com/advisories/GHSA-fc5q-39xc-vjpf) lists no patched
version, although the npm audit response recommends `>=4.0.8`.

Only `lib/parse.js` differs from the original runtime. The backport follows the
bare-star guard proposed in [upstream PR 235](https://github.com/micromatch/picomatch/pull/235)
(unmerged), with additional handling for a literal `[` inside a character class
and an unterminated character class. The guard also accounts for `nobracket` and
`unescape`, which can make an otherwise quoted star active. A repeated extglob containing a glob star
is passed through the existing risky-extglob literalization path. This blocks
the nested unbounded quantifiers behind CVE-2026-112366, including `+(*)c`,
`+(*a)`, `+(a*)b`, `+(**a)` and nested/multiple-branch variants. It preserves the
existing `maxExtglobRecursion: false` explicit opt-out; callers must not disable
the safeguard for untrusted patterns.

Compatibility change: affected `+(...)` and `*(...)` patterns match their literal
text instead of repeating a wildcard. For example, `+(*)c` matches `+(*)c`, not
`abc`. Ordinary Vite file globs, escaped stars with default options, simple
character classes, and fixed alternatives retain their existing behavior.
Complex quoted or POSIX-class repeated patterns containing a literal star may
also be conservatively literalized. This does not claim to make every
possible arbitrary regex/glob pattern resistant to ReDoS.

`scripts/lib/picomatch-security.test.mjs` is part of `pnpm check`. It checks the
actual Vite/tinyglobby/fdir dependency resolution, normal file-glob behavior,
and malicious cases in a child process with a hard timeout. `pnpm audit` remains
enabled without advisory exclusions. Registry advisory services cannot identify
the contents of a local patched package; these tests and this provenance record
are required alongside the audit.

Remove the override and this directory when an official release fixing this
advisory is published and passes the same regressions. Until then, review future
picomatch advisories against this vendored code manually; do not infer that a
clean registry audit covers local modifications.
