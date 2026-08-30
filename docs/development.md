# Development

Postly intentionally has no GitHub Actions. Keep the important checks runnable on a local machine:

~~~bash
cargo xtask fmt
cargo xtask lint
cargo xtask test
cargo xtask check
cargo xtask compat
cargo xtask bench
cargo xtask fuzz
cargo xtask package
~~~

For low-disk environments:

~~~bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo xtask check
~~~

The base/ directory is an ignored research corpus. Verify it is not staged before committing:

~~~bash
git check-ignore base
git status --ignored --short
~~~

Do not place tokens, real environment values, customer data or private certificates in fixtures. Use local deterministic servers for network integration tests as the protocol surface grows.

`cargo xtask bench` measures real local import and workspace/search operations.
Keep generated JSON under the ignored `bench-generated/` directory and record
machine, revision and methodology before sharing a result.

`cargo xtask compat` executes every checked-in Postman collection/environment and
OpenAPI fixture. It reports fixture execution separately from request mapping:
manual-review requests remain counted as imported but are excluded from the
fully-supported mapping score. This is fixture evidence, not a claim of full
Postman behavioral parity.

`cargo xtask package` builds a locked release locally and creates ignored
`dist/` artifacts with SHA-256 checksums. See [packaging](packaging.md) for the
boundary of that command.

The CLI environment command stores values locally and only prints the environment name and count, never the values:

~~~bash
postly env set --workspace ./project --name Local --set baseUrl=http://127.0.0.1:8080 --secret token=replace-me
~~~
