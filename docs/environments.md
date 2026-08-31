# Environments and variables

Postly keeps environment data in the local project. The CLI, native desktop
workspace and collection runner share the same variable model, so a request
behaves the same way when it is edited, sent or run headlessly.

## Variable scopes

When a name is defined in more than one scope, the current precedence is:

```text
iteration data > runtime > request > environment > collection > project > globals
```

Iteration data is read-only from scripts. Runtime values are session-scoped and
are not written back to an environment file. An unresolved variable is reported
before network I/O rather than being sent as a literal `{{name}}` placeholder.

## Local layout

An environment is a readable TOML file under the workspace:

```text
my-api/
├── postly.toml
├── collections/
└── environments/
    ├── local.postly-env.toml
    └── local.example.toml
```

Keep real values in ignored local files and commit a redacted `.example.toml`
template when a team needs a shared starting point. Environment files preserve
the enabled/disabled state of each variable.

## Plain values and secrets

Use a plain value for non-sensitive configuration:

```bash
postly env set --workspace ./my-api --name Local \
  --set baseUrl=http://127.0.0.1:8080
```

For credentials, use the OS credential store and persist only an opaque,
workspace-scoped reference:

```bash
postly env set --workspace ./my-api --name Local \
  --secret API_TOKEN="$API_TOKEN"
```

When a value must not appear in shell history or process arguments, pipe it to
the command:

```bash
printf '%s\n' "$API_TOKEN" | postly env set --workspace ./my-api \
  --name Local --secret-stdin API_TOKEN
```

Imported legacy plaintext values are not guessed to be secrets. Migrate them
explicitly:

```bash
postly env migrate --workspace ./my-api --name Local --key API_TOKEN
postly env migrate --workspace ./my-api --name Local --all
```

`--all` migrates only variables that the import marked as secret. If the
platform credential store is unavailable, a secret operation fails instead of
silently creating a new plaintext value.

## Import and sharing checklist

1. Import the environment into the workspace with `--secure` when its Postman
   `secret` entries should go directly to the OS credential store, then inspect
   the JSON report.
2. Confirm the active environment name before sending a request.
3. Migrate credentials with `--secret` or `env migrate`.
4. Keep `.postly/` runtime artifacts and real environment files private.
5. Share only a redacted example file and never paste a token into a fixture,
   log, issue or benchmark.

The native GUI masks existing credential-backed values. Leaving an existing
secret field blank preserves its reference; removing the variable deletes it.
See [privacy and security boundaries](privacy.md) for the storage and logging
model.
