---
name: firecrawl-mcp-competitive-intel
description: Compare competitors' pricing, plans, features, and changelogs with the self-hosted Firecrawl, and re-run the comparison later to see what changed. Use for competitor snapshots and change reviews.
---

# Competitive intelligence

Uses the tools and limits described in the `firecrawl-mcp` skill. Adapted from Firecrawl's `firecrawl-competitive-intel` workflow (ISC).

Scheduled monitoring is not available on this instance. Instead, produce a dated snapshot that can be re-run and compared.

## Collect

1. For each competitor find the pricing, features/product, changelog or release-notes, and blog pages (search or map).
2. For pricing pages, use a JSON scrape with a schema for plan name, price, currency, billing period, key limits, and notable features - only the JSON enters the chat. Spot-check one competitor by reading its page yourself, and read the page yourself whenever the JSON looks incomplete.
3. For changelogs and feature pages, use the `summary` or `query` format ("changes in the last 3 months?"); read in full only the pages that matter for the conclusion. In terminal hosts save full pages to `.firecrawl/intel/<competitor>/<page>-<YYYY-MM-DD>.md` so later runs can compare.
4. If a previous snapshot exists (earlier files, or a table the user pastes), compare and list only real changes.

## Deliverable

```markdown
# Competitive snapshot - [date]

## Pricing
| Competitor | Plan | Price | Period | Key limits | Source |

## Features and positioning
[Per competitor, sourced]

## Recent changes
[From changelogs, or diff against the previous snapshot]

## Implications
[What this means for the user - labeled as analysis]

## Re-run
competitors: [list with URLs]
pages: [pricing, changelog, ...]
```

## Quality bar

- Prices exactly as shown, with currency and period; note if they depend on region or login.
- Mark a change only when both snapshots show it.
