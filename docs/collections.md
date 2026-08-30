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

The native workspace can duplicate a saved request with a new stable UUID and a
`copy` name, or delete a request file after validating that it is a request
under the current workspace's `collections/` tree. Draft requests are not
deleted, and storage guards reject paths outside the project.

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
