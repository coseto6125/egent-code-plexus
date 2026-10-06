# LLM agents and tools

Deepens rung 8. Read it when the diff touches a tool the model can call, a prompt that carries retrieved content, or the code that turns model output into an action.

This surface is younger than the rest, and it inverts a habit: the model is not a trusted component. Everything it emits is attacker-influenced input, because everything it read may have been attacker-supplied.

## Map it first

```bash
ecp find "tool executor schema" --mode bm25
ecp impact --target <tool_dispatch_fn> --direction down --depth 3    # what a tool call can reach
ecp pattern -p 'def $NAME($$$ARGS)' --callers-of <ToolRegistry.execute> --lang py
```

Enumerate the callable tools, then read each one's argument list. The argument list is the attack surface.

## Checks

1. **A model-produced argument carries data, never authority.** Tenant id, org slug, account id, bot id, role, price, discount, user id: these reach the tool from the turn's session, and the tool reads them there. A tool that accepts `org_id` as a parameter will one day be called with the wrong one — not by an attacker, just by the model guessing.
2. **The tool re-checks entitlement itself.** The prompt is not a permission boundary. A tool reachable in a turn runs with whatever that turn's session holds, so the check belongs in the tool.
3. **Retrieved content is untrusted input.** Crawled pages, knowledge-base documents, previous customer messages and tool results all enter the context window. Any of them can carry instructions. The system prompt cannot out-argue them, so the defence is that no instruction can reach an action the session did not already permit.
4. **Content and instructions are separable in the prompt.** Retrieved passages sit inside a delimited block the template controls, not concatenated into the instruction text.
5. **The tool set is scoped to the caller.** An admin or configuration tool is registered only for a turn whose session already holds that role. Registering it always and filtering by prompt is a permission check the model performs.
6. **A write tool is idempotent or guarded.** Models retry. A tool that charges, sends, or deletes states how a repeat is handled.
7. **Model output that becomes a link, a redirect, or markup is escaped where it lands.** See [`untrusted-render.md`](untrusted-render.md).
8. **The turn has a ceiling.** Tool-call depth, total tokens, wall-clock. Without one, a loop the model enters is a cost incident. See [`cost-and-limits.md`](cost-and-limits.md).
9. **Tool arguments are validated against the schema before execution**, not after, and a schema violation ends the call rather than being repaired into a plausible value.

## Known-safe — do not report these

| Shape | Why it is fine |
|---|---|
| A tool taking a free-text `query` or `keywords` | Search terms are data; the injection risk is at the sink, not the argument |
| A tool taking an id the model got from a previous tool result **in the same turn**, where the tool re-scopes it to the session's tenant | The re-scope is the check; verify it exists in the query |
| A system prompt that tells the model what not to do | Weak, but not itself a vulnerability. File it only if it is the *only* control |
| A tool registered for every turn whose every action is already tenant-scoped | Scoping the registry is defence in depth, not a requirement |
| Retrieved content interpolated into a prompt with no escaping | There is no escaping for a prompt. The control is at the action boundary, not the string |

## Failure scenario, worked

> An admin tool accepted `org_id` in its schema. The model filled it from conversation context and picked the wrong tenant, writing another org's bot configuration. No attacker was needed to trigger it; a crafted message makes it reliable. Precondition: a turn where the tool is registered. Authority obtained: configuration write on an arbitrary tenant.
