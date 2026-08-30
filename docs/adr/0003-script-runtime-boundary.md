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

Execution is opt-in with `postly send --scripts` or `postly run --scripts`.
The context exposes only the currently documented `pm.*` subset, and the VM
has a two-second synchronous limit. Variable updates are applied to the
current execution session and are not silently written back to environment
files.

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
request storage or runner semantics.
