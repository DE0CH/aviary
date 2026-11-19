# YYC AI refactor: pigeon, magpie, hermit, supervisor

The agreed design for replacing ui (the per-person CloudCLI router) and web with pigeon, magpie, hermit and the
sandbox supervisor (Rust, public repo DE0CH/aviary), written 2026-11-02.

- TARGET.md: the target design as agreed with Deyao in chat.
- DECISIONS.md: Deyao's decisions on the open questions (authoritative over everything else here).
- DESIGN.md: the merged design (text of record); spec-*.md: the component specs it overrides where they differ.
- WORK-PACKAGES.md: the implementation plan.
- switch-final.md, deploy-workflows.md, pg-ha.md, gap-*.md: inputs, partly superseded (see their banners).
- talos-apps-move.md: the move of everything that is not YYC AI (news stream, mail merge, AI watch, Postgres with HA)
  to yyc-talos: T4-T6 with the nodes per step, written 2026-11-05.
