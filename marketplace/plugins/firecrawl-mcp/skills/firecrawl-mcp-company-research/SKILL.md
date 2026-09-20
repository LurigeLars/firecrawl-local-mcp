---
name: firecrawl-mcp-company-research
description: Research companies with the self-hosted Firecrawl - a pre-meeting brief on one company (products, customers, news, hiring, key people in their public business roles) or a structured list of companies from public directories and listing pages.
---

# Company research and company lists

Uses the tools and limits described in the `firecrawl-mcp` skill. Adapted from Firecrawl's `firecrawl-lead-research` and `firecrawl-company-directories` workflows (ISC).

## A. Brief on one company

1. Scrape the company's own pages: home, about, product, pricing, customers, careers, press. Use the `query` or `summary` format for pages where you need only a fact or the gist; read the most important one or two pages in full.
2. Search recent news (last 12 months), funding, launches, partnerships.
3. For named people, use only public professional information (role, talks, interviews, company bios). Do not collect private details (home address, family, personal contact data).

```markdown
# Brief: [Company]
## Overview        [what they do, size and stage signals, customers]
## Recent activity [dated, sourced]
## Key people      [public roles only]
## Talking points  [5-7 specific, evidence-based]
## Likely needs    [hypotheses, labeled as such]
## Sources
```

## B. List of companies from a directory

1. Scrape the directory page. If it paginates with plain URLs (`?page=2`, `/page/3`), scrape each page directly; map can help discover them. Filters, infinite scroll, and click-to-load cannot be driven here - say so and use what is reachable.
2. Extract rows into a table: name, website, category, location, short description, source URL. Use a JSON scrape with an array schema on each directory page (the local model does the extraction, so page text stays out of the chat); read one page yourself to spot-check. The local model sees only about the first 20,000 characters of a page, so for long directory pages use the site's paginated URLs or read the page yourself.
3. Dedupe by website domain. Optionally enrich the top entries by scraping their homepages.

Output a markdown table (or CSV/JSON if asked) plus counts: pages scraped, rows found, rows after dedupe.

## Quality bar

- No invented fields - blank means not found.
- Respect the site's terms; keep volume modest.
