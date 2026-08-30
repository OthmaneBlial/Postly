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

Environment files may contain secrets. Postly ignores runtime environment files by default; keep local values there and commit a separate redacted template when sharing a project:

~~~text
environments/staging.postly-env.toml
environments/staging.example.toml
~~~

The current CLI reads enabled environment values and resolves scopes in this order:

~~~text
runtime > request > environment > collection > project > globals
~~~

Undefined variables fail before a request is sent, which prevents an accidental request to a literal {{missing}} URL.
