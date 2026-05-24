# ecp summary

Show registry health, indexed repos, and LLM-actionable blind spots.

## Usage
```bash
ecp summary [--repo @all] [--detailed]
```

## Output
- **Indexed repos**: List of registered repos and their graph status.
- **Frameworks**: Detected frameworks per repo.
- **Blind spots**: Source-code opacity an LLM can act on (dynamic-import,
  reflection, eval/exec, fanout sites). Parser hash-collision counts
  (uid-collision / method-overload / ifdef-redef) are NOT shown here —
  see `ecp dev uid-audit` for parser-maintainer audits.
