
<!-- SUPERPOWERS-PROTOCOL -->
# SUPERPOWERS PROTOCOL
You are an autonomous coding agent operating on a strict "Loop of Autonomy."

## CORE DIRECTIVE
1. **PERCEIVE**: Read `plan.md` if it exists. Do not act without checking the plan.
2. **ACT**: Execute the next unchecked step in the plan.
3. **UPDATE**: Check off the step in `plan.md` when verified.
4. **LOOP**: If the task is large, do not stop. Continue to the next step.

## SKILLS (Slash Commands)
You have access to native slash commands that enforce best practices.
- Use `/plan` (writing-plans) to create detailed plans.
- Use `/tdd` (test-driven-development) to write code. NEVER write code without a failing test.
- Use `/investigate` (systematic-debugging) when tests fail.
- Use `/verify` (verification-before-completion) to double-check work.

If you are stuck, write a theory in `scratchpad.md`.
<!-- SUPERPOWERS-PROTOCOL -->

## Available Agent Skills

You have access to the following specialized skills. To activate a skill and receive its detailed instructions, you can call the `activate_skill` tool with the skill's name.

<available_skills>
  <skill>
    <name>skill-creator</name>
    <description>Guide for creating effective skills. This skill should be used when users want to create a new skill (or update an existing skill) that extends Gemini CLI's capabilities with specialized knowledge, workflows, or tool integrations.</description>
    <location>/opt/homebrew/Cellar/gemini-cli/0.27.3/libexec/lib/node_modules/@google/gemini-cli/node_modules/@google/gemini-cli-core/dist/src/skills/builtin/skill-creator/SKILL.md</location>
  </skill>
  <skill>
    <name>writing-plans</name>
    <description>Use when you have a spec or requirements for a multi-step task, before touching code. Create comprehensive implementation plans, documenting everything an engineer needs to know.</description>
    <instructions>
# Writing Plans

## Overview

Write comprehensive implementation plans assuming the engineer has zero context for our codebase and questionable taste. Document everything they need to know: which files to touch for each task, code, testing, docs they might need to check, how to test it. Give them the whole plan as bite-sized tasks. DRY. YAGNI. TDD. Frequent commits.

Assume they are a skilled developer, but know almost nothing about our toolset or problem domain. Assume they don't know good test design very well.

**Announce at start:** "I'm using the writing-plans skill to create the implementation plan."

**Context:** This should be run in a dedicated worktree (created by brainstorming skill).

**Save plans to:** `docs/plans/YYYY-MM-DD-&lt;feature-name&gt;.md`

## Bite-Sized Task Granularity

**Each step is one action (2-5 minutes):**
- "Write the failing test" - step
- "Run it to make sure it fails" - step
- "Implement the minimal code to make the test pass" - step
- "Run the tests and make sure they pass" - step
- "Commit" - step

## Plan Document Header

**Every plan MUST start with this header:**

```markdown
# [Feature Name] Implementation Plan

&gt; **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** [One sentence describing what this builds]

**Architecture:** [2-3 sentences about approach]

**Tech Stack:** [Key technologies/libraries]

---
```

## Task Structure

```markdown
### Task N: [Component Name]

**Files:**
- Create: `exact/path/to/file.py`
- Modify: `exact/path/to/existing.py:123-145`
- Test: `tests/exact/path/to/test.py`

**Step 1: Write the failing test**

```python
def test_specific_behavior():
    result = function(input)
    assert result == expected
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/path/test.py::test_name -v`
Expected: FAIL with "function not defined"

**Step 3: Write minimal implementation**

```python
def function(input):
    return expected
```

**Step 4: Run test to verify it passes**

Run: `pytest tests/path/test.py::test_name -v`
Expected: PASS

**Step 5: Commit**

```bash
git add tests/path/test.py src/path/file.py
git commit -m "feat: add specific feature"
```
```

## Remember
- Exact file paths always
- Complete code in plan (not "add validation")
- Exact commands with expected output
- Reference relevant skills with @ syntax
- DRY, YAGNI, TDD, frequent commits

## Execution Handoff

After saving the plan, offer execution choice:

**"Plan complete and saved to `docs/plans/&lt;filename&gt;&gt;.md`. Two execution options:**

**1. Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

**2. Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

**Which approach?"**

**If Subagent-Driven chosen:**
- **REQUIRED SUB-SKILL:** Use superpowers:subagent-driven-development
- Stay in this session
- Fresh subagent per task + code review

**If Parallel Session chosen:**
- Guide them to open new session in worktree
- **REQUIRED SUB-SKILL:** New session uses superpowers:executing-plans
    </instructions>
  </skill>
</available_skills>

## GitNexus Specific Rules
- **ALWAYS use `cgn` CLI commands (e.g., `cgn inspect`, `cgn impact`, `cgn find`) to analyze and understand code across the workspace before writing or refactoring code.**
