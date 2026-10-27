"""Classify a `server.exec` command as an observation or an effect.

Why this exists
---------------
`server.exec` is registered with `risk_level="destructive"` because the registry
cannot know what an arbitrary shell command does. That declaration is correct for
authorization: every call still needs an exact per-call approval. It is wrong for
*reporting*, because the stage derivation used the declared risk and therefore
reported a run whose only effectful call was `df -i` as having entered `change`.

The live acceptance run made that concrete: `date -Is` moved the run to `change`
before anything had been written, and a later `test -e` produced a second
`verify -> change` flip. The operator sees "changed the server" for commands that
read two files.

Scope and guarantees
--------------------
* **Display only.** The result of this module never authorizes, rejects, approves
  or executes anything. Authorization stays with the tool registry, the policy
  engine and the host's one-shot ticket. A `True` from here only means "this run
  is still collecting evidence about the target".
* **Fails closed.** Anything this module does not positively recognise is
  reported as effectful, because claiming a read that was really a write is the
  dangerous direction of error. The allowlist is deliberately small: pure readers
  with no subcommand that could mutate.
* **Not a shell parser and not a sandbox.** Quoting, substitution and aliases are
  not resolved. `r''m -rf /` is not recognised as a delete; it is simply not
  recognised as a read either, so it stays effectful. A classifier bug here can
  never widen permission.
"""
from __future__ import annotations

import re

#: A prefix added by the model-facing shell policy; it is not part of the command.
_SET_FLAGS = re.compile(r"^set -[a-z]+\n")

#: First tokens that only ever read. Deliberately excludes tools with mutating
#: subcommands (`systemctl`, `docker`, `find`, `journalctl`, `ssh`, `env`), even
#: though their common invocation is a read.
_READ_ONLY_COMMANDS = frozenset({
    "cat", "catv", "cut", "date", "df", "du", "echo", "free", "grep", "head",
    "hostname", "id", "ls", "lsblk", "nproc", "printf", "ps", "pwd", "sort",
    "ss", "stat", "tail", "test", "tr", "uniq", "uname", "uptime", "wc", "whoami",
})

#: Characters that turn a list of reads into something else. A pipeline or a
#: redirect can write a file even when every command name reads.
_UNSAFE_CHARACTERS = ("|", ">", "<", "`", "$(")

#: Segment separators that keep every segment a separate command word list.
_SEGMENT_SEPARATOR = re.compile(r"&&|\|\||;|\n")


def is_read_only_command(command: object) -> bool:
    """True only when the whole command is a chain of recognised pure readers.

    The function is intentionally conservative; see the module docstring.
    """
    if not isinstance(command, str) or not command.strip():
        return False
    text = _SET_FLAGS.sub("", command.strip())
    # Split on the logical separators first: `&&` and `||` are themselves made of
    # characters an unsafe-marker scan would reject, and a chain of readers is
    # still only readers. Every segment is then checked on its own.
    segments = [segment.strip() for segment in _SEGMENT_SEPARATOR.split(text)]
    if any(marker in segment for segment in segments for marker in _UNSAFE_CHARACTERS):
        return False
    if not segments or any(not segment for segment in segments):
        return False
    for segment in segments:
        first = segment.split()[0]
        # A bare `VAR=value cmd` assignment prefix hides the real command name.
        if "=" in first and not first.startswith("-"):
            return False
        if first not in _READ_ONLY_COMMANDS:
            return False
    return True
