# Authentication

Postly keeps authentication in the local request model and resolves variable
references only when a request runs. Current HTTP authentication includes:

- No auth
- Basic auth
- Bearer token
- API key in a header or query parameter
- OAuth 2.0 Client Credentials
- OAuth 2.0 Authorization Code + PKCE (loopback browser callback or explicit/manual code exchange)
- OAuth 2.0 Refresh Token
- OAuth 2.0 Device Authorization Grant (RFC 8628)

## OAuth 2.0 Client Credentials

The Client Credentials flow obtains an access token from the configured token
endpoint, then sends it on the API request as `Authorization: <token_type>
<access_token>`. Token requests use an `application/x-www-form-urlencoded`
body with `grant_type`, `client_id`, `client_secret` and the optional `scope`.

In a saved request, the native model looks like this:

```toml
[auth]
type = "oauth2_client_credentials"
token_url = "{{oauthTokenUrl}}"
client_id = "postly-local"
client_secret = "{{oauthClientSecret}}"
scope = "read:users"
```

The GUI exposes these fields in the Auth tab. A Postman `oauth2` auth block
with `grant_type=client_credentials`, an access-token URL, client ID and client
secret is imported and exported.

## OAuth 2.0 Authorization Code + PKCE

Postly supports a local loopback browser flow as well as an explicit/manual
code exchange. When the authorization code is empty, the shared HTTP engine
generates a verifier and state in memory, opens the authorization URL through
the caller's system browser, validates the loopback callback, and exchanges the
returned code locally. Imported requests with an existing code and verifier
continue to use the explicit exchange path.

```toml
[auth]
type = "oauth2_authorization_code_pkce"
authorization_url = "https://auth.example.test/authorize"
token_url = "https://auth.example.test/token"
client_id = "postly-local"
redirect_uri = "http://127.0.0.1:8787/callback"
code = "{{oauthCode}}"
code_verifier = "{{oauthCodeVerifier}}"
client_secret = "{{oauthClientSecret}}" # optional for confidential clients
scope = "read:users"
```

For the browser flow, use an HTTP loopback redirect on `localhost`,
`127.0.0.1`, or `::1` with an explicit port. A port of `0` asks Postly to bind
an ephemeral local port; the provider must support the resulting loopback URI
and the redirect URI sent to authorization and token endpoints must be
registered according to that provider's rules. The callback listener accepts a
single bounded GET request, checks `state`, and returns a static success or
failure page without exposing codes or verifiers. The CLI opts in with
`--oauth-browser`; the GUI opts in automatically when the Authorization code
field is empty. The verifier, state, authorization code and access token are
memory-only.

Example CLI invocation:

~~~bash
postly request https://api.example.test/profile \
  --oauth-token-url https://auth.example.test/token \
  --oauth-authorization-url https://auth.example.test/authorize \
  --oauth-client-id postly-local \
  --oauth-redirect-uri http://127.0.0.1:0/callback \
  --oauth-scope read:users \
  --oauth-browser
~~~

The PKCE verifier must be 43–128 RFC 7636 unreserved characters. Tokens are
cached only in memory for the current HTTP engine, keyed to the explicit code
exchange inputs, and never written to the workspace, history or logs.

## OAuth 2.0 Refresh Token

Refresh-token requests use the configured token endpoint with
`grant_type=refresh_token`, the client ID, the refresh token and optional client
secret/scope:

```toml
[auth]
type = "oauth2_refresh_token"
token_url = "https://auth.example.test/token"
client_id = "postly-local"
refresh_token = "{{oauthRefreshToken}}"
scope = "read:users"
```

The GUI exposes the token URL, client ID, refresh token and scope. The CLI
accepts the same flow with `--oauth-refresh-token` alongside the OAuth token
URL/client ID options.

Tokens are cached only in memory inside the current HTTP engine and only when
the response supplies an `expires_in` value longer than the refresh safety
window. The cache is not written to the workspace, history or logs. Client
secrets should normally be supplied through ignored environment files or
variable references; Postly does not claim OS keychain storage yet.

## OAuth 2.0 Device Authorization Grant

Device Code authentication is intended for providers where the user approves
the client on a separate device or browser. Configure the device authorization
endpoint, token endpoint and client ID; a client secret and scope are optional:

```toml
[auth]
type = "oauth2_device_code"
device_authorization_url = "https://auth.example.test/oauth/device"
token_url = "https://auth.example.test/oauth/token"
client_id = "postly-local"
scope = "read:users"
```

At runtime Postly displays the verification URL and user code, then polls the
token endpoint locally. The polling honors `authorization_pending` and
`slow_down`, is bounded by the provider's `expires_in` (maximum 24 hours), and
keeps the device code/access token in memory only. The CLI prints the
verification instructions to stderr; the GUI shows clickable verification
links in the response panel. The device code itself is never displayed or
written to the workspace.

Token endpoint errors are reported without echoing the token response body or
secret values. Responses are bounded before parsing, and missing or malformed
`access_token` fields fail the request clearly.

OAuth token orchestration currently applies to HTTP requests. WebSocket,
SSE-specific token orchestration and AWS Signature V4 remain planned.
