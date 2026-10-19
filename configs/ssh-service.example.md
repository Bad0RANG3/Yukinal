# `server_exec_denylist` in `ssh-service.example.json`

JSON cannot carry comments, so this note explains the `server_exec_denylist` key
shown in [`ssh-service.example.json`](./ssh-service.example.json). The operator
documentation is [`docs/security.md`](../docs/security.md); this file is only a
reading aid for the example.

## Where it goes

- The **root** list is service-level. Every target inherits it, and the entries
  are matched **before** that target's own entries.
- A **target** may add its own list. It is appended to the inherited
  service-level entries; it never replaces them and cannot remove one. A target
  without the key still gets the service-level list.
- Both lists come from the same key name, `server_exec_denylist`.

## Entry forms

```json
"server_exec_denylist": [
  "rm -rf /",
  {"pattern": "shutdown", "regex": true, "case_insensitive": true}
]
```

- A bare string is a **literal**, matched as a substring of the normalized
  command, case-sensitive.
- An object uses `pattern` plus two optional booleans: `regex` (default `false`)
  and `case_insensitive` (default `false`). `{"pattern": "..."} ` alone is a
  literal.
- Limits: at most 256 entries; a pattern is at most 1024 UTF-8 bytes and must be
  non-empty after trimming.
- An invalid list (wrong type, too many entries, an empty or oversized pattern,
  an unknown key, a non-boolean flag, or a regex that does not compile) fails
  configuration parsing. The service does not start with the blacklist silently
  missing.

## Matching

The command and every literal pattern are first normalized: each run of
whitespace (`space`, tab, CR, LF, form feed, vertical tab) collapses to one
space, then leading and trailing spaces are trimmed. Literals are substring
matches on that normalized command; `case_insensitive` lowercases both sides.
Regex entries are ECMAScript regex searches over the normalized command.

## What it is not

This is a defense-in-depth guardrail on the *raw* command string, not a sandbox
and not a shell parser. It does not expand shell syntax, so quoting and
substitution obfuscation such as `r''m -rf /`, `rm${IFS}-rf${IFS}/` or piping a
base64-decoded command can bypass it. It is an additional deny layer in front of
the approval flow, never a substitute for it.
