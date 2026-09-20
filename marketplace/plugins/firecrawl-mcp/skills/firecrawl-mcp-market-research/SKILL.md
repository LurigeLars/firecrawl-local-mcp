---
name: firecrawl-mcp-market-research
description: Sourced market, company, earnings, and industry research with the self-hosted Firecrawl - market size and players, company financial summaries, comparison tables, and trends, with every number tied to a source, period, and unit.
---

# Market research

Uses the tools and limits described in the `firecrawl-mcp` skill. Adapted from Firecrawl's `firecrawl-market-research` workflow (ISC).

Infer market or companies, data focus, timeframe, and geography from context. Ask at most 1-3 short questions only if blocked.

## Collect

1. Search for primary sources first: investor-relations pages, annual and interim reports, regulator filings (SEC EDGAR, Finansinspektionen, etc.), official statistics, then reputable news.
2. Numbers you will cite: read the source page yourself (terminal hosts: save it to a file and read the relevant section). Broad sweeps across many companies: use a JSON scrape with a small schema (metric, value, unit, currency, period, source date) so only the numbers enter the chat, then verify every figure that ends up in a conclusion against its page. Report figures are often deep in long pages, which the local model does not see - read those yourself.
3. Interactive charts, period selectors, and logged-in portals cannot be driven here. Look for a static page, press release, or PDF link with the same numbers instead; if none exists, report the gap.

## Deliverable

```markdown
# Market research: [market or companies]

## Overview
[What the market is, key players, size and growth - each with source]

## Company profiles
[Business, latest reported figures, recent developments]

## Comparison
| Company | Metric | Value | Unit/currency | Period | Source |

## Trends and risks
[Evidence-backed, dated]

## Sources
[URL - what was taken from it - date shown on page]
```

## Quality bar

- Every number has period, unit, currency, and source. Missing values stay "not found"; never estimate silently.
- Cross-check key figures against a second source when possible; note conflicts.
- Page retrieval time is not the data date - use the date shown in the source.
- Research only: no investment advice, no buy/sell recommendations.
