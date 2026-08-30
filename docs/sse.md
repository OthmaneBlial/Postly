# Server-Sent Events

Postly can subscribe to an SSE endpoint from the CLI and decode events as the
HTTP body arrives. The connection uses the shared Rust HTTP engine, so custom
headers and bearer/basic authentication follow the same rules as regular
requests.

~~~bash
postly sse https://api.example.test/events
postly sse https://api.example.test/events --output-json
postly sse https://api.example.test/events -H 'Authorization: Bearer local-token'
~~~

The parser handles `id`, `event`, repeated `data` lines, comments, `retry`,
LF/CRLF line endings, chunk boundaries, and a final event without a trailing
blank line. JSON output emits one event object per line, which can be piped to
local tools without an account or hosted relay. Reconnection policy and a
dedicated GUI event console remain planned.
