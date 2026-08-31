# Migration from Postman

The current migration path is deliberately explicit:

~~~text
export a Postman Collection v2.1
  -> postly import collection collection.json --output ./project
  -> postly import environment environment.json --output ./project
  -> postly list ./project
  -> inspect the JSON migration report and review warnings
  -> postly send <request>.postly.toml
~~~

For projects that keep local overrides in a dotenv file, import it into the
same workspace with an explicit environment name. Postly keeps `${VAR}` and
other placeholders literal; it does not expand values or execute shell syntax:

~~~bash
postly import dotenv .env --output ./project --name Local
postly import dotenv .env --output ./project --name Local --secret API_TOKEN
~~~

Only keys passed with `--secret` are written to the OS credential store. All
other imported values remain in the ignored local environment file, so Postly
does not guess which variables are sensitive.

The importer preserves collection metadata, folders, request URLs, query parameters, headers (including disabled and scalar values), descriptions, raw/JSON/urlencoded/form-data/file bodies, common auth types including Basic, Digest, OAuth 2.0 Client Credentials, Authorization Code + PKCE, Refresh Token, Device Code and AWS Signature V4, examples, variables and request-level scripts. Collection variables accept scalar values and the `current` fallback; disabled collection variables are not activated and are reported explicitly. Collection and folder auth is materialized into requests that do not override it, so the imported files retain the effective behavior without depending on a hidden runtime tree. Unsupported auth types and other review-worthy fields are reported and counted for manual review rather than silently discarded.

Collection and folder pre-request/test events are preserved into the native collection/request files in execution order, whether Postman exports `script.exec` as a line array or a single source string. With `--scripts`, the current local Node.js bridge executes the preserved source and reports basic assertions; without that explicit flag, scripts remain source-only.

Postly can also export the native collection and environment back to Postman
JSON:

~~~bash
postly export collection ./project --output ./project.postman.json
postly export environment ./project --name Local --output ./local.postman.json
~~~

The export covers the current native model and is intended as a practical
interoperability path, not a claim of perfect Postman round-trip fidelity.

Current limitations:

- Script execution is opt-in and currently depends on a local Node.js installation; the bridge is a tested prototype, not an embedded or hostile-code sandbox. On newer Node versions its permission model is enabled for defense in depth. A bounded `pm.sendRequest` callback slice is available, but it uses Node's native fetch rather than Postly's proxy/TLS/cookie transport.
- Authorization Code + PKCE, Refresh Token, Client Credentials and Device Code are supported in the HTTP engine. PKCE can use the explicit imported code/verifier exchange or an opt-in CLI / automatic GUI loopback-browser callback; the provider's redirect registration and login remain external requirements.
- GraphQL request metadata is retained in the structured native body model.
- File paths should be checked after import because their meaning depends on the source project location.
- Collection-level and folder-level script inheritance is materialized into request source, while variable persistence and broader runtime behavior remain limited.
- Export back to Postman covers the current model; full pm.* compatibility and a measured behavioral score remain planned.

The current compatibility matrix is machine-readable in
compat/postman-script-compatibility.json. Entries are marked supported,
partial or planned only when the corresponding runtime behavior and regression
evidence justify that status.

Treat the report as part of the migration artifact. A successful JSON parse is not proof that a collection is behaviorally compatible.

For individual requests, cURL can be imported without executing a shell:

~~~bash
postly import curl "curl -X POST https://api.example.test/users -H 'Content-Type: application/json' --data-raw '{\"name\":\"Ada\"}'" --output ./project
~~~

The current parser covers common method, URL, headers, JSON/raw data, cookies, Basic Auth and GET data options. Unsupported flags produce a warning or an explicit parse error.

In the native workspace, use the command palette's **Import cURL** action to
paste the same kind of command into an unsaved request draft. After editing,
**Copy cURL** places a POSIX-shell-quoted command on the clipboard; runtime-only
authentication such as OAuth client credentials stays visible as a warning
instead of being silently materialized.
