# WebSocket

The CLI has a local, bidirectional WebSocket slice for `ws://` and `wss://`
endpoints. It reuses the explicit local workflow: custom headers and bearer or
basic authentication can be supplied on the command line, text messages can be
sent during connection setup, and incoming text/binary messages are emitted as
they arrive.

~~~bash
postly websocket ws://127.0.0.1:9000/socket --send '{"type":"ping"}' --reconnect 3
postly ws wss://api.example.test/stream --output-json
postly websocket ws://127.0.0.1:9000/socket --bearer local-token
~~~

JSON output uses one object per line with `type` and `data`; binary and pong
payloads are base64-encoded. Ping frames are answered with pong frames, and the
CLI exits cleanly on a server close. `--reconnect N` gives bounded retries for
failed handshakes or server-initiated closes, resending the configured messages
on each new connection. The timeout is an inactivity timeout for the receive
loop.

The native GUI exposes `Connect WS` for the current request. It supports
`ws://` and `wss://`, request headers/auth/cookies/query parameters, text sends,
binary/ping/pong/close frame visibility, connection status and a bounded
500-message console history. Reconnect policy and saved message presets remain
future slices.
