# Scripts and tests

Postly preserves Postman pre-request and test source during import. Execution
is explicit:

~~~bash
postly send ./project/collections/api/requests/health.postly.toml --scripts
postly run ./project --scripts --reporter json
~~~

The native workspace exposes the same sources in its `Scripts` tab. Saving a
request writes the pre-request and post-response/test text back to the
canonical `.postly.toml` file; the GUI currently edits and preserves scripts
but does not execute them automatically.

The current prototype is a Rust-controlled, no-shell Node.js bridge. It uses a
short-lived `node:vm` context with a two-second synchronous execution limit.
The supported compatibility slice includes:

- `pm.environment.get/set/unset/has/clear/replaceIn`
- `pm.collectionVariables.get/set/unset/has/clear/replaceIn`
- `pm.globals.get/set/unset/has/clear/replaceIn`
- `pm.iterationData.get/has/replaceIn` (read-only)
- `pm.variables.get/set/unset/has/clear/replaceIn`, including iteration data precedence
- `pm.request` URL, method and headers with `get`, `has`, `add`, `upsert` and `remove`
- `pm.response.code/status/responseTime/text/json/headers/cookies`
- `pm.test` and `pm.expect` equality, inclusion, property, boolean, numeric, type, regex and negated checks
- `pm.response.to.be.ok`, response header checks and `pm.response.cookies.get`
- `console.log`, `console.warn` and `console.error` capture

Script output is kept local. CLI output reports assertions but deliberately
does not print captured console logs, because a script can log a secret. The
bridge rejects source larger than 512 KiB, caps the serialized input payload at
4 MiB, and starts Node with a minimal child environment containing only the
current `PATH`. The Rust worker enforces a three-second process deadline and
caps each stdout/stderr pipe at 8 MiB; captured output is also capped at 200 log
entries of 4 KiB each and 1,000 test results. These limits are resource guards,
not a security boundary for hostile code. Filesystem, network and process
permissions still require a future embedded-runtime or isolated-worker
decision.
`pm.sendRequest`, OAuth helpers and broad Postman parity remain planned.
