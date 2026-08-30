# Developer console

The native GUI includes a local `Console` response tab for request debugging.
It keeps a bounded in-memory stream of:

- request, gRPC, GraphQL introspection, SSE and WebSocket lifecycle events;
- script start, completion, warnings, errors and `console.log` output;
- cancellation and worker-failure diagnostics.

The console is deliberately not a network trace recorder. Postly avoids
printing full request URLs, bodies or headers in its own messages. Script output
is checked against the current resolved request and variable context, and
matching values are replaced with `[redacted]` before the message is retained.
The console is cleared from the `Clear console` action and is not written to the
workspace.

The console does not sandbox JavaScript. The opt-in Node bridge and its
filesystem/process/environment boundaries are documented in
[`scripting.md`](scripting.md); treat script output as user-controlled and do
not intentionally print credentials.
