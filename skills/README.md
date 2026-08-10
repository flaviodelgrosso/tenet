# Loops built-in worker skills

Loops ships only language-agnostic procedural skills. Product intent belongs in the specification; language/framework/company/domain knowledge belongs to user-provided skills.

## Default mapping

| Worker role | Built-in skill |
|---|---|
| Architect | `spec-analysis` |
| Reconcile | `spec-analysis` |
| Implement | `implementation` |
| Repair | `debugging` |
| Review | `code-review` |
| Assess | `spec-assessment` |

## Design principles

- **Evidence first:** repository state and deterministic checks outrank agent claims.
- **Fresh-context friendly:** important knowledge is written into structured handoffs/evidence, not assumed from conversation history.
- **Minimal role overlap:** implementation, debugging, review, and final specification assessment have different contracts.
- **Language agnostic:** Loops never decides what language-specific skill a project needs.
- **Progressive disclosure:** each `SKILL.md` carries the essential workflow; detailed rubrics live under `references/` and are read only when relevant.
- **No verification replacement:** skills guide reasoning; deterministic Loops gates still decide executable verification.

## Layout

```text
skills/
├── code-review/
│   ├── SKILL.md
│   └── references/
├── debugging/
│   ├── SKILL.md
│   └── references/
├── implementation/
│   ├── SKILL.md
│   └── references/
├── spec-analysis/
│   ├── SKILL.md
│   └── references/
└── spec-assessment/
    ├── SKILL.md
    └── references/
```

Each skill uses standard Agent Skills-compatible YAML frontmatter with explicit `name` and task-oriented `description`.
