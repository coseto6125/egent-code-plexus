# verify-resolver report (py)

Oracle records: 677
Gnx records: 4299

## Summary

| class | count |
|---|---|
| TP (correct) | 34 |
| FP_ghost (wrong target) | 78 |
| FP_overmatch (Global with alts) | 1 |
| FN_dangling (oracle resolved, gnx didn't) | 42 |
| tier_demoted (correct but fell back to Global) | 34 |
| oracle_only (oracle imports gnx never saw) | 475 |
| gnx_only same-file (excluded from diff) | 619 |
| gnx_only other (no oracle counterpart) | 3316 |

## Per-tier breakdown (gnx side)

| tier | TP | FP_ghost | FP_overmatch | tier_demoted | FN_dangling |
|---|---|---|---|---|---|
| Global | 34 | 76 | 1 | 34 | 0 |
| SameFile | 0 | 2 | 0 | 0 | 0 |
| Unresolved | 0 | 0 | 0 | 0 | 42 |
| oracle | 0 | 0 | 0 | 0 | 0 |

## Top-20 worst offenders

| src_file | name | class | detail |
|---|---|---|---|
| docs/conf.py | packaging | oracle_only_resolved |  |
| src/flask/typing.py | annotations | oracle_only_resolved |  |
| src/flask/typing.py | cabc | oracle_only_resolved |  |
| src/flask/typing.py | t | oracle_only_resolved |  |
| src/flask/__init__.py | json | oracle_only_resolved |  |
| src/flask/__init__.py | Flask | oracle_only_resolved |  |
| src/flask/__init__.py | Blueprint | oracle_only_resolved |  |
| src/flask/__init__.py | Config | oracle_only_resolved |  |
| src/flask/__init__.py | after_this_request | oracle_only_resolved |  |
| src/flask/__init__.py | copy_current_request_context | oracle_only_resolved |  |
| src/flask/__init__.py | has_app_context | oracle_only_resolved |  |
| src/flask/__init__.py | has_request_context | oracle_only_resolved |  |
| src/flask/__init__.py | current_app | oracle_only_resolved |  |
| src/flask/__init__.py | g | oracle_only_resolved |  |
| src/flask/__init__.py | request | oracle_only_resolved |  |
| src/flask/__init__.py | session | oracle_only_resolved |  |
| src/flask/__init__.py | abort | oracle_only_resolved |  |
| src/flask/__init__.py | flash | oracle_only_resolved |  |
| src/flask/__init__.py | get_flashed_messages | oracle_only_resolved |  |
| src/flask/__init__.py | get_template_attribute | oracle_only_resolved |  |
