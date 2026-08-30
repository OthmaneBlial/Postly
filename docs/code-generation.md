# Code generation

Postly can turn a saved request into source code for a neighboring application
or a bug report:

~~~bash
postly snippet ./my-api/collections/my-api/requests/health.postly.toml \
  --language python
~~~

The generator reads the same canonical method, URL, query parameters, enabled
headers, cookies, authentication metadata and body that the CLI and GUI use.
Supported targets are:

| Language | Output |
| --- | --- |
| curl | POSIX-shell command |
| javascript | browser/Node fetch |
| python | requests |
| rust | async reqwest |
| go | net/http |
| java | java.net.http.HttpClient |
| csharp | HttpClient |
| php | cURL extension |

Postly deliberately keeps {{variables}} visible. It does not resolve an
environment or fetch OAuth tokens just to print source, and it does not read
secret values from the operating-system credential store for this command.
Basic auth and OAuth client credentials produce warnings so a developer can
decide how to materialize them safely. Multipart and binary bodies also carry
review warnings where a target language needs local adaptation.

Use --output-json when another local tool needs both the generated source and
the warning list:

~~~bash
postly snippet ./request.postly.toml --language rust --output-json
~~~

Generated snippets are starting points for review, not claims that every
language dependency setup, TLS policy or runtime is configured for a specific
project.
