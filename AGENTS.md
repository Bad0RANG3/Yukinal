# SSH security invariants

Read the actual Python caller, desktop host, SSH service and frontend call chain before changing an SSH tool. The current implementation is C++/libssh2, not the removed Rust/Tauri stack.

- New remote command branches must use structured operations and `shell_quote` for **every** variable argument (including workdirs, environment assignments and descriptive text). Reviewed `server.exec` shell text is one explicitly approved script passed as a quoted `sh -c` argument; it is never PTY input. Add hostile semicolon, quote, backtick, newline and leading-option regression cases when extending a branch.
- Reject leading `-` target/host aliases at every configuration boundary. No local ssh/scp process is currently used. Any future approved binary integration must use an argv array, `--`, disable ControlMaster/ControlPath/ControlPersist and cannot accept model-supplied options.
- All SFTP entry points, including Agent writes and manual transfers, resolve paths on the existing authenticated connection and enforce configured roots. Do not add a probe or error that reveals paths outside those roots. Keep symlink and missing-parent regression tests.
- The PTY worker accepts trusted manual UI input only. AI stdin is disabled. An AI PTY feature requires a host-issued, single-use ticket bound to run, ChangePlan step, complete input, target and absolute expiry for **each** write, plus C++ enforcement before it can ship. A session grant or source label supplied by a model is never sufficient.
- Every pending SSH execution has a non-reusable cancellation identity and absolute deadline. Approval restore must not extend a ticket. Test cancellation before dispatch, during remote execution, and replay after expiry.
- Remote outputs, errors, archives and goal-review evidence reach models only through bounded, sanitized untrusted envelopes. Output cannot authorize a mutation; auto mode needs a new human decision after external evidence. Keep the native host enforcement aligned.
- Terminal writes pass the stateful SGR-only filter before rendering. Never bypass it for cached or split output. Markdown strips ANSI. Theme updates must retain the complete palette and resize must reach the remote PTY.
- Audit at the actual SSH write boundary with source and execution/session identity. Do not record plaintext credentials or keystrokes; secret buffers and temporary environment maps are cleared on success, exceptions and cancellation.

Reviewers must trace every new branch through both Python and C++ and verify its negative tests. Run `pnpm check` and `git diff --check`; report platform or live-SSH limitations honestly.
