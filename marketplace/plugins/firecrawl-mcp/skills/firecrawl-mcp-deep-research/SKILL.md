---
name: firecrawl-mcp-deep-research
description: Produce a sourced research report from live web evidence with the self-hosted Firecrawl - market, technology, company, policy, or "what is known about X" questions that need several sources, cross-checking, and citations.
---

# Deep web research

Uses the tools and limits described in the `firecrawl-mcp` skill.

## Plan

1. Restate the question in one line and list 3-6 sub-questions. Ask at most one clarifying question, only if the scope is truly ambiguous.
2. For each sub-question run one search (limit 5-8). Vary wording; add a year or site filter when freshness or authority matters.
3. Pick the strongest 2-4 sources per sub-question: primary sources (official sites, filings, standards, papers' landing pages) before news, news before blogs and forums.
4. Scrape the chosen pages as main-content markdown and read them yourself - this is careful work, so accuracy beats budget here. In terminal hosts save them to `.firecrawl/research/` and read only the relevant sections. For supporting pages where you need a single fact, use the `query` format instead of the full page.
5. Record each claim with its URL. When sources disagree, keep both and note which is more authoritative or more recent.
6. Stop when every sub-question has at least two independent sources or you have clearly shown the evidence is thin.

Academic paper databases (the hosted "research index") are not available here; search the open web and scrape publisher or arXiv landing pages instead.

## Deliverable

```markdown
# [Question]

## Answer
[3-6 sentences, direct]

## Findings
### [Sub-question]
- [Claim] ([source](url))

## Disagreements and uncertainty
[Conflicts, gaps, stale data]

## Sources
[URL - publisher - date if shown - what it supported]
```

## Quality bar

- Every factual sentence traces to a cited URL; label your own inferences.
- Prefer dates shown on the page; never invent a publication date.
- Say "not found" rather than filling gaps.
