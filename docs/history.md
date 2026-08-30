# Local request history

Postly records saved-request executions in `.postly/history.jsonl`. Each record
contains only a timestamp, request name, method, sanitized URL, status, duration
and outcome. Query values, headers, cookies, bodies, authentication and response
content are intentionally excluded.

The CLI reads newest entries first and supports local filtering:

~~~bash
postly history ./project --search users --method GET
postly history ./project --status 200
postly history ./project --errors-only
postly history ./project --clear
~~~

The file is bounded to the newest 1,000 entries and approximately 1 MiB. The
bound prevents an unattended local project from accumulating unlimited history.
The native GUI shows the same recent entries, filters them by text, can clear
the metadata file, and reopens a matching saved request by its stable request
UUID. Legacy entries without an identity remain metadata-only and cannot be
reopened automatically.
