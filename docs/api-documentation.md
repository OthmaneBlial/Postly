# Local API documentation

Postly can turn a native collection into deterministic Markdown without a
cloud service:

~~~bash
postly docs ./my-api --output ./my-api/API.md
~~~

The generated document is designed for local previews, repository review and
sharing through the user's existing Git or static-site workflow. It includes:

- collection and request descriptions;
- methods, URLs and folder paths;
- parameter, cookie and header names with enabled/disabled state;
- authentication and body-type labels;
- assertion counts and response-example status metadata.

Security is conservative by default. Header values, authentication material and
response-example bodies are omitted. URLs preserve variables and ordinary
parameters, but query values whose names look sensitive (for example token,
secret or password) are replaced with [redacted]. Descriptions are user-authored
Markdown and should still be reviewed before publication.

Use --include-example-bodies only when the output is intended to contain those
bodies:

~~~bash
postly docs ./my-api --include-example-bodies --output ./my-api/API.md
~~~

Included bodies are bounded and marked with a warning in the generated file.
Postly does not publish or deploy the result; output remains on the local
filesystem unless the user explicitly commits or hosts it.
