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

The importer preserves collection metadata, folders, request URLs, query parameters, headers, descriptions, raw/JSON/urlencoded/form-data/file bodies, common auth types, examples, variables and request-level scripts. Unsupported or review-worthy fields are reported rather than silently discarded.

Collection and folder pre-request/test events are preserved into the native collection/request files in execution order. They remain source-only until a script runtime is selected and tested.

Current limitations:

- Scripts are preserved as source but are not executed yet.
- GraphQL request metadata is retained as a reviewable raw JSON body.
- File paths should be checked after import because their meaning depends on the source project location.
- Collection-level and folder-level Postman scripts/auth inheritance are not yet modeled as executable runtime behavior.
- Export back to Postman, full pm.* compatibility and a measured behavioral score are planned.

The current compatibility matrix is machine-readable in compat/postman-script-compatibility.json. Every entry is marked planned until an actual runtime and regression fixture prove it.

Treat the report as part of the migration artifact. A successful JSON parse is not proof that a collection is behaviorally compatible.
