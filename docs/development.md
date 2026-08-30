# Development

Postly intentionally has no GitHub Actions. Keep the important checks runnable on a local machine:

~~~bash
cargo xtask fmt
cargo xtask lint
cargo xtask test
cargo xtask check
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
