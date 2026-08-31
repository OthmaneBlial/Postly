# Scripts and tests

Postly preserves Postman pre-request and test source during import. Execution
is explicit:

~~~bash
postly send ./project/collections/api/requests/health.postly.toml --scripts
postly run ./project --scripts --reporter json
~~~

The native workspace exposes the same sources in its `Scripts` tab. Saving a
request writes the pre-request and post-response/test text back to the
canonical `.postly.toml` file. The GUI offers explicit local preview buttons
for each script type. A separate session-only `Run pre-request and
post-response scripts when sending` toggle enables the same bridge in the
HTTP send worker: pre-request mutations affect that send, and post-response
tests/logs appear in the Scripts panel and developer console. It is disabled
by default, never changes the saved request or environment files, and a
post-response script failure does not hide the received response.

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
- `pm.sendRequest` callback requests for bounded HTTP(S) subrequests with
  headers, raw bodies and response text/JSON
- `pm.test` and `pm.expect` equality, inclusion, property, boolean, numeric, type, regex and negated checks
- `pm.response.to.be.ok`, response header checks and `pm.response.cookies.get`
- `console.log`, `console.warn` and `console.error` capture

The tested expectation subset also includes deep equality with stable object-key
comparison, lengthOf, keys, oneOf, empty, numeric at.least/at.most/within and
the a/an type aliases. This remains a compatibility slice rather than a claim
of complete Chai or Postman assertion parity.

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
`pm.sendRequest` is intentionally a partial compatibility slice: it permits up
to eight HTTP(S) callback requests per script, caps each response at 1 MiB and
uses a two-second request timeout. Those subrequests use Node's native fetch,
so Postly proxy, custom-CA, mTLS, OAuth and cookie-jar parity does not apply to
them yet. OAuth helpers, broader Postman parity and an embedded or isolated
runtime remain planned. The current Node bridge is still intended for source
the user intentionally runs locally; its VM is not a security boundary.
