# ecp path

The route between two symbols: the ordered chain plus the edge that makes each
hop.

`ecp impact` walks out from one symbol and lists the set it reaches. `ecp path`
joins two, and names what sits between them — the question behind "why does
changing A break B" and "how does this handler end up touching the database".

Cypher's `-[:Calls*1..N]->` answers only whether the pair is connected: the
walk keeps the endpoints and drops the route.

## Usage

```bash
ecp path <FROM> <TO> [--direction down] [--depth 8] [--repo <PATH>]
```

- `<FROM>` / `<TO>`: bare symbol name or the `Owner.Method` FQN form.
- `--direction`: `down` (FROM calls … calls TO — the default), `up` (FROM is
  called by … called by TO), `both` (ignore edge direction).
- `--depth`: maximum hops searched. Default 8.
- `--relation_types calls,extends`: restrict which edges may be steps. Default
  is every non-containment relation.
- `--include-tests`: allow test files on the route. Off by default, so a
  production path is not routed through a test helper that calls both ends.
- `--min-confidence 0.8`: drop low-confidence edges. Default 0.0 is
  recall-first, matching `ecp impact`. Values outside 0.0–1.0 are rejected
  rather than filtering every edge into a `found: false` you cannot tell from
  a real miss.
- `--from-file` / `--to-file`: substring on the file path, to pin an
  overloaded endpoint to one definition.
- `--include-heuristic`: allow heuristic edges (MirrorsField, EventTopicMirror)
  as steps.

## Reading the output

```
ecp path handler db_query
path[4]{filePath,kind,line,name,requiresVerification,viaConfidence,viaReason,viaRelType}:
  chain.py,Function,1,handler,false,1,"",""
  chain.py,Function,4,service,false,1,call,calls
  chain.py,Function,7,repo_lookup,false,1,call,calls
  chain.py,Function,10,db_query,false,1,call,calls
```

`viaRelType`, `viaReason` and `viaConfidence` describe the edge INTO that step.
The first row is the start node, which has no incoming edge: its relation and
reason are empty and its confidence reads 1. `hops` is one less than the number
of rows.

Read `viaRelType` before drawing a conclusion — the default walk follows every
non-containment relation, so a path is not necessarily a call chain:

```
ecp path makeChild TBase
  mixed.ts,Function,3,makeChild,false,1,"",""
  mixed.ts,Class,2,TChild,false,1,type_annotation,accesses
  mixed.ts,Class,1,TBase,false,1,heritage,extends
```

"makeChild reaches TBase" here means one type reference and one inheritance
edge, not two calls. Pass `--relation_types calls` when the question really is
about the call graph.

An overloaded name is not an error. Every candidate for both names seeds the
same walk, so the cost is one traversal rather than one per pair, and
`fromCandidates` / `toCandidates` report how many definitions each name had.
The chain's own file:line says which two the answer joined. To ask about a
specific one, narrow with `--from-file` / `--to-file`.

Two or more definitions of a name also mean the resolver suppressed every bare
call to it at index time, so edges are missing from the walk. The payload says
so in its caveat, and a miss under that condition is a lower bound rather than
an answer.

## Misses

`found: false` with no `path` array is a real answer, not a missing index. The
`result` caveat says how to widen the search, and — when the arguments were
simply the wrong way round — names the direction that does work:

```
result: "no downstream path from 'db_query' to 'handler' within 8 hops. Widen
with --depth, --direction both, --include-tests or --include-heuristic; an
unreachable pair is a real answer, not a missing index. There is a path in the
other direction (upstream, 3 hops): rerun with --direction upstream."
```

## Heuristic edges are off by default

The reverse of `ecp impact`, on purpose. `impact` can include heuristic edges
because it buckets them separately and tags them `requires_verification`; a
path has no second bucket — a step is in the chain or it is not. So the default
answer is the one that is safe to act on. `--include-heuristic` opts in, marks
every inferred step `requiresVerification: true`, and adds a caveat naming how
many hops are inferred.

## Shortest only

One route is returned: the shortest under the current filters. Enumerating
alternatives explodes on a real repo, and "how are these connected" is answered
by one concrete chain. To see whether a *different* route exists, narrow with
`--relation_types` or raise `--min-confidence` and run again.
