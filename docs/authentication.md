# Authentication

Postly keeps authentication in the local request model and resolves variable
references only when a request runs. Current HTTP authentication includes:

- No auth
- Basic auth
- Bearer token
- API key in a header or query parameter
- OAuth 2.0 Client Credentials

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
secret is imported and exported. Other OAuth grant types remain manual-review
items so authorization-code browser flows are never misrepresented as
supported.

Tokens are cached only in memory inside the current HTTP engine and only when
the response supplies an `expires_in` value longer than the refresh safety
window. The cache is not written to the workspace, history or logs. Client
secrets should normally be supplied through ignored environment files or
variable references; Postly does not claim OS keychain storage yet.

Token endpoint errors are reported without echoing the token response body or
secret values. Responses are bounded before parsing, and missing or malformed
`access_token` fields fail the request clearly.

OAuth 2.0 Client Credentials currently applies to HTTP requests. WebSocket,
SSE-specific token orchestration, Authorization Code/PKCE, Device Code,
refresh-token workflows and AWS Signature V4 remain planned.
