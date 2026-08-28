---
name: klepto-code-understanding
description: Model-independent source analysis using Klepto repo maps, lexical and symbol search, language servers, structural search, clone detection, and static analysis.
---

# Klepto code understanding

Use the cheapest tool that can falsify the current hypothesis.

1. Cold start: read `.klepto/index/repo-map.md` if present. It is orientation, not proof.
2. Exact identifier, error, or path: use `rg`.
3. Keyword search where implementations should outrank comments/tests: use `cs` when installed.
4. Known symbol definition/references: use the workspace language server or SCIP; never use embeddings as proof of a call edge.
5. Code shape or mechanical rewrite: use `ast-grep`; use Semgrep for policy or security patterns.
6. Multi-site edit or extraction: use `dcd --file <path> --format json` when installed.
7. Natural-language intent after lexical search fails: use the configured semantic backend, then confirm every hit with source reads and lexical/symbol tools.

Klepto search returns ranked JSON with `backend`, `provenance`, and `freshness`.
Keep large search/SARIF outputs under `.klepto/artifacts/<session-id>/` and read only the relevant ranges.
Before finishing an edit, inspect `git diff`, run relevant tests, and verify changed call sites.
