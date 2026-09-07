# Your first API project

This example uses fictional orders and a real HTTP server bound only to your
machine. No account, credentials, Rust, Python or internet API is required when
using the packaged binaries.

## In the desktop app

1. Open Postly and choose **Try the example**. Choose an empty folder to keep it.
2. Select **01 Health**, then **Send**. The response contains `"status": "ok"`.
3. Select **02 List orders**, then **Send**. Two paid orders appear.
4. Change the `status` query parameter to `pending` and send again: one order.
5. Save the request. Your folder now contains readable `.postly.toml` files.
6. Open the Assertions tab to inspect the checks. The collection also works
   with `postly run /path/to/your/example` while the example API is running.

The built-in server runs while that desktop session stays open. The saved
workspace remains on disk. To restart its API later, use
`postly demo --serve-only --port PORT`, using the port in the collection's
`baseUrl`. If the port is occupied, choose another and update `baseUrl`.

## From the terminal

```bash
# Create a new example and start its server (leave this terminal open).
postly demo ./orders-demo --port 3979

# In a second terminal:
postly-gui ./orders-demo
postly run ./orders-demo
postly run ./orders-demo --reporter json
```

If using source instead of a release, build with Rust 1.95.0 and run
`cargo run -- demo ./orders-demo --port 3979`; the GUI command is
`cargo run -p postly-app -- ./orders-demo`.

## Version your requests

```bash
cd orders-demo
git init
git add .
git commit -m "Add Orders API requests"
# Change a query parameter in Postly and save.
git diff
```

The List orders request includes a saved response example. Use
`postly mock ./orders-demo --port 3980` (from its parent folder) to serve that
snapshot at `/orders`; this mock always returns the saved example, whereas the
demo API dynamically filters by `status`.

Press Ctrl+C to stop a CLI demo server. Postly refuses to create an example in
a nonempty directory, so rerunning it cannot overwrite your saved changes.

## Try migration

The source repository also includes `examples/orders/postman.json`. Start
`postly demo --serve-only --port 3979`, then import that file into a new folder:

```bash
postly import collection examples/orders/postman.json --output ./imported-orders
postly-gui ./imported-orders
```

The imported collection preserves its response example and has no scripts.
The built-in starter created by `postly demo` additionally contains native
assertions; add equivalent checks in the imported requests' Assertions tab.
