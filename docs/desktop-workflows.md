# Import and test from the desktop

These workflows are available in source builds after the onboarding milestone.
They are not included in the old `v0.1.0` download.

## Import a collection

Open or create a workspace, then choose **Import collection…** in the project
toolbar. Choose Postman v2.1 or OpenAPI 3.0/3.1, select a JSON/YAML file and choose
a new or empty destination folder. The GUI accepts source files up to 32 MiB.

**Import and review** runs the shared importer in a background worker. The
result reports the imported request count and retains the importer's warnings,
including the affected request or operation when available. Importing does not
send requests or execute scripts. Local OpenAPI references follow the shared
importer's bounded file rules; remote references remain warnings in this GUI
flow. The CLI also offers explicit remote-reference resolution.

Choose **Open imported project** after reviewing the report. Save existing
drafts and finish any active collection run before switching. The destination
remains on disk if you dismiss the report. Existing nonempty destinations are
rejected rather than merged or overwritten.

Try `examples/orders/postman.json` with `postly demo --serve-only --port 3979`
running in a terminal. It contains two requests and a saved response example,
using fictional data and no scripts.

## Run a collection

Select a collection and environment in the sidebar, then choose **Run
collection…**. The runner reads the saved request files; unsaved tab edits are
excluded. An optional folder filter includes nested folders. **Stop after the
first failure** ends the run on the first failed request or assertion.

Scripts are disabled by default. Enabling them requires Node.js and executes
the imported sources through the same bridge as the CLI; only run trusted
scripts. This is not a hostile-code sandbox.

The run continues in the background, leaving the editor responsive. **Cancel
run** cancels active requests through the shared cancellation token. The result
shows HTTP statuses, durations, native assertion failures and script test
results. Select a request name to reopen it, correct it, save and rerun.

**Export JSON report…** saves the result only when you choose a destination.
Review diagnostic text before sharing it; assertion messages may include
values from your API. Response bodies are not added to the report by this UI.

For automation, the same saved collection works with `postly run ./my-api`
and the CLI's JSON/JUnit reporters. See [the CLI guide](cli.md) for iteration
data, concurrency and advanced transport options.

## Compare JSON responses

Send a request, then choose **Compare JSON…** in the project toolbar. Use a
saved response example from that request or load a JSON file as the baseline.
Select **Compare current response** to inspect added, removed and changed
fields. Paths use JSON Pointer escaping (`~1` for `/`, `~0` for `~`); arrays
are compared by index. Missing properties and explicit `null` are distinct.

Exclude volatile subtrees by entering one JSON Pointer per line, such as
`/timestamp` or `/meta/requestId`. The UI shows how many exclusions matched.
Exclusions control comparison; they are not a redaction mechanism for sharing
private responses. A change of a field's type is reported at that field.

The comparison accepts up to 4 MiB per input and bounds depth, work and the
number of reported changes. A partial result is explicitly labeled and cannot
establish equality. The report describes the response captured when you pressed
Compare; press it again after a new request. Status and headers are outside this
JSON body comparison.

Baselines and comparison results stay in memory and are discarded when you
close the comparison window. No response file is created automatically. A
saved response example is still ordinary project data, so only save payloads
you intend to retain and potentially commit.
