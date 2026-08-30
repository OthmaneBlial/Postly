# Local collections

Postly keeps canonical project data in readable files so a collection can be reviewed with normal Git tools.

~~~text
my-api/
  postly.toml
  collections/
    users/
      postly.collection.toml
      requests/
        list-users.postly.toml
        admin/
          delete-user.postly.toml
  environments/
    local.postly-env.toml
~~~

Request files are independent merge units. The collection file stores metadata and collection-scoped variables; request files store method, URL, parameters, headers, body, auth, scripts and examples.

The native GUI Body tab can edit raw text, JSON and GraphQL envelopes as well as
URL-encoded fields, multipart text/file parts and binary file uploads. File
contents are read at send time; only the project-relative or absolute path is
stored in the request file.

The native workspace can duplicate a saved request with a new stable UUID and a
`copy` name, or delete a request file after validating that it is a request
under the current workspace's `collections/` tree. Draft requests are not
deleted, and storage guards reject paths outside the project.

Changing a saved request's name or folder also relocates its canonical file;
the request UUID remains stable and the old path is removed only after the new
file has been written successfully.

Environment files can contain legacy plaintext values, but new --secret values
are stored in the operating-system credential store. The Git-native file then
contains only an opaque workspace-scoped reference. Postly ignores runtime
environment files by default; commit a separate redacted template when sharing
a project:

The native GUI exposes `＋ New environment` and `Edit selected` actions beside
the environment selector. It manages plain values in the local TOML file,
preserves disabled flags, masks existing keychain-backed values and sends newly
entered secret values through the operating-system credential store. Leaving an
existing secret field blank preserves its opaque reference; remove the variable
when it should no longer exist.

~~~text
environments/staging.postly-env.toml
environments/staging.example.toml
~~~

Store a secret without writing its value to the environment file:

~~~bash
postly env set --workspace ./my-api --name Local \
  --secret API_TOKEN="$API_TOKEN"
~~~

For a secret that must not appear in shell history or process arguments, pipe
one value per `--secret-stdin` key:

~~~bash
printf '%s\n' "$API_TOKEN" | postly env set --workspace ./my-api --name Local \
  --secret-stdin API_TOKEN
~~~

To migrate an imported or legacy plaintext value in place, name it explicitly:

~~~bash
postly env migrate --workspace ./my-api --name Local --key API_TOKEN
~~~

`postly env migrate --all` migrates only variables marked `secret` by an import;
it does not guess that ordinary values such as `baseUrl` are credentials. The
command preserves each variable's enabled flag and writes the environment file
only after the value has been stored successfully.

The keychain namespace is derived from the canonical workspace path. Moving or
copying a workspace therefore requires setting its secrets again; a reference
from another workspace is rejected. Older environment files without a
secret_ref field remain readable and resolve their existing plaintext values
for migration compatibility.

The current CLI reads enabled environment values and resolves scopes in this order:

~~~text
iteration data > runtime > request > environment > collection > project > globals
~~~

Iteration data is supplied by the collection runner and is read-only from
scripts through `pm.iterationData`. Runtime values remain mutable for the
current execution session.

Undefined variables fail before a request is sent, which prevents an accidental request to a literal {{missing}} URL.
