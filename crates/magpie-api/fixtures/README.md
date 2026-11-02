# magpie-api golden fixtures

The JSON of magpie's contract, pinned. `tests/fixtures.rs` round-trips every file through the contract types (parse,
serialise, compare: no field may change); `tests/client_server.rs` sends every route fixture through the client to a
server built from the `Server` trait and reads the answers back; `tests/shapes.rs` checks the keys pigeon (pigeon
spec 14.1) and the supervisor (supervisor spec 10-11) rely on and that no key outside Claude Code's own data is
snake_case.

- `routes/<route id>.json`, one per entry of `routes::ROUTES`:
  `{"route", "params"?: {name: string}, "query"?: {…}, "request"?: {…} | "requestBytes"?: "…",
  "headers"?: {…}, "responses": [{"status", "body"? | "bytes"?}]}`. Every route has at least one success (a status
  of the route's `ok` list) and at least one error (the flat envelope with a known code answered with that status).
  The client/server test answers the first success, then the first error.
- `events/*.json`: every event kind; `rows/*.json`: every row kind (and the tool row's variants); `changes/*.json`:
  every change type; `commands/*.json`: every command type; `live/*.json`: the live state.

A change to the contract changes the types, these files and the fakes in one commit (WORK-PACKAGES conventions).
Emails are `example.com`; ids and hashes are made up.
