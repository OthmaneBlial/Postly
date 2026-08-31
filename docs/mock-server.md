# Local mock server

Postly can serve saved response examples as a deterministic local HTTP server.
This is useful for frontend work, demos and offline development when the real
API is unavailable or should not receive test traffic.

```bash
postly mock ./my-api --port 3000
```

Use `--environment NAME` to resolve the selected environment's placeholders in
route paths, response headers and response bodies before serving the examples:

```bash
postly mock ./my-api --environment Local --port 3000
```

The command accepts a workspace directory or one collection directory. It
loads every saved response example below that scope and derives one route from
the request method and URL path. Query parameters are ignored when matching, so
`GET /users?fixture=local` uses the same example as `GET /users`. The first
matching example wins when a request has multiple examples.

```bash
# Serve only one collection
postly mock ./my-api/collections/users --port 3001

# Bind one request and exit; useful in a smoke test
postly mock ./my-api --port 3002 --once
```

The response uses the saved status (default `200`), saved status text when
present, enabled headers and body.
Saved response-example status text is used as the reason phrase when present.
Saved response-example cookies are emitted as `Set-Cookie` headers, including
their supported `Domain`, `Path`, `SameSite`, `Expires`, `Max-Age`, `Secure` and
`HttpOnly` attributes. Cookie names/values and attributes containing unsafe
line breaks are skipped rather than written to the response. If no content
type is saved, Postly sends `text/plain; charset=utf-8`. A saved `delay_ms`
makes the mock wait before responding. Header names and values that
contain CR/LF are skipped to avoid response-header injection. Unknown routes
return a generic `404` JSON response and do not echo the requested URL.
Without `--environment`, collection variables are still used and unresolved
placeholders remain literal; the selected environment adds the local values
without writing to the workspace.

After sending a saved request in the desktop app, use **Save as example** in the
response toolbar to add or replace a named fixture in that request. The app
requires the request to be saved first and rejects binary response bodies for
this text-based fixture format.

The mock server is local-only by default (`127.0.0.1`), does not contact a
Postly service, and does not mutate request files, environments or history.
It is an HTTP fixture server, not a full simulation of authentication,
streaming protocols or server-side application behavior. Richer protocol
fixtures remain on the roadmap.
