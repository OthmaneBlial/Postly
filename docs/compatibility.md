# Compatibility status

Compatibility numbers are not published until they come from executable fixtures. The current evidence is the fixture in compat/postman-import/ and the importer unit coverage.

| Area | Status | Evidence |
| --- | --- | --- |
| Postman Collection v2.1 JSON parsing | working slice | importer tests and fixture |
| folders and request files | working slice | filesystem round-trip and importer tests |
| variables and environments | working slice | variable precedence tests and environment import |
| common headers, bodies and auth | working slice | model/import coverage |
| Postman scripts | preserved, not executed | migration report and docs |
| Postman pm.* runtime | planned | no runtime claimed |
| collection runner | sequential HTTP slice | CLI run |
| Postman behavioral parity | not measured | no percentage claimed |

Any future score must count semantic cases, exclude placeholders and retain failing fixtures as regressions.
