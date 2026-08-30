# Scripts and tests

Postly preserves Postman pre-request and test source during import. Execution
is explicit:

~~~bash
postly send ./project/collections/api/requests/health.postly.toml --scripts
postly run ./project --scripts --reporter json
~~~

The current prototype is a Rust-controlled, no-shell Node.js bridge. It uses a
short-lived `node:vm` context with a two-second synchronous execution limit.
The supported compatibility slice includes:

- `pm.environment.get/set`
- `pm.collectionVariables.get/set`
- `pm.variables.get/set/replaceIn`
- `pm.request` URL, method and readable headers
- `pm.response.code/status/responseTime/text/json/headers`
- `pm.test` and `pm.expect` equality, inclusion, property and boolean checks
- `console.log`, `console.warn` and `console.error` capture

Script output is kept local. CLI output reports assertions but deliberately
does not print captured console logs, because a script can log a secret. The
bridge does not provide a security boundary for hostile code; filesystem,
network and process permissions still require a future embedded-runtime or
isolated-worker decision. `pm.sendRequest`, OAuth helpers and broad Postman
parity remain planned.
