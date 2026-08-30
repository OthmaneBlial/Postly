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

The importer preserves collection metadata, folders, request URLs, query parameters, headers, descriptions, raw/JSON/urlencoded/form-data/file bodies, common auth types, examples, variables and request-level scripts. Collection and folder auth is materialized into requests that do not override it, so the imported files retain the effective behavior without depending on a hidden runtime tree. Unsupported or review-worthy fields are reported rather than silently discarded.

Collection and folder pre-request/test events are preserved into the native collection/request files in execution order. With `--scripts`, the current local Node.js bridge executes the preserved source and reports basic assertions; without that explicit flag, scripts remain source-only.

Current limitations:

- Script execution is opt-in and currently depends on a local Node.js installation; the bridge is a tested prototype, not an embedded or hardened sandbox.
- GraphQL request metadata is retained as a reviewable raw JSON body.
- File paths should be checked after import because their meaning depends on the source project location.
- Collection-level and folder-level script inheritance is materialized into request source, while variable persistence and broader runtime behavior remain limited.
- Export back to Postman, full pm.* compatibility and a measured behavioral score are planned.

The current compatibility matrix is machine-readable in compat/postman-script-compatibility.json. Every entry is marked planned until an actual runtime and regression fixture prove it.

Treat the report as part of the migration artifact. A successful JSON parse is not proof that a collection is behaviorally compatible.

For individual requests, cURL can be imported without executing a shell:

~~~bash
postly import curl "curl -X POST https://api.example.test/users -H 'Content-Type: application/json' --data-raw '{\"name\":\"Ada\"}'" --output ./project
~~~

The current parser covers common method, URL, headers, JSON/raw data, cookies, Basic Auth and GET data options. Unsupported flags produce a warning or an explicit parse error.
