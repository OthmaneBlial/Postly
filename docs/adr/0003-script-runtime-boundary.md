# ADR 0003: Explicit script runtime boundary

## Status

Accepted for an opt-in compatibility prototype.

## Context

Postman exports frequently contain JavaScript pre-request and test scripts.
Postly needs executable evidence before publishing `pm.*` compatibility claims,
but choosing an embedded engine is a separate portability and security
decision.

## Decision

The first runtime is a Rust-controlled Node.js subprocess. Rust serializes a
request, active variable scopes and (for tests) a response to the child stdin;
the child runs a short-lived `node:vm` context and returns structured updates,
tests and captured logs over stdout. Postly never invokes a shell and does not
place the script payload in the process argument list.

Execution is opt-in with `postly send --scripts`, `postly run --scripts`, or the
session-only GUI Scripts toggle. The GUI applies pre-request mutations only to
the in-flight request and retains the response when post-response scripts
fail; it never persists those runtime mutations back to workspace files.
The context exposes only the currently documented `pm.*` subset, and the VM
has a two-second synchronous limit. The compatibility contract currently
covers scoped variable reads/mutations, read-only iteration data, request
header mutation, response inspection, `pm.test`, `pm.expect` and captured
console methods. Source larger than 512 KiB is rejected before spawning Node;
`NODE_OPTIONS` and `NODE_PATH` are removed from the child environment. Variable
updates and unsets are applied to the current execution session and are not
silently written back to environment files. The Rust worker also enforces a
three-second process deadline, while the harness caps captured logs at 200
entries of 4 KiB each and test results at 1,000 entries.

## Security boundary

Node's VM is not a security boundary for hostile JavaScript. The runtime is
therefore suitable only for source the user intentionally runs locally. The
CLI does not print captured console logs because scripts may log secrets.
Filesystem, network, process and cancellation permissions must be solved by a
future embedded engine or isolated worker before scripts are enabled by
default.

## Consequences

This provides real compatibility fixtures and honest failure behavior without
adding a large native dependency immediately. It also makes Node.js an
explicit local prerequisite for this prototype. A future embedded runtime can
reuse the `ScriptResult` contract and compatibility tests without changing
request storage or runner semantics. The contract deliberately carries
variable removals separately from assignments so `unset` and `clear` do not
silently become no-ops at the Rust boundary.
