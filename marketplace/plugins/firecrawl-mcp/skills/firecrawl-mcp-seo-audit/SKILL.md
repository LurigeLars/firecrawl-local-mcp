---
name: firecrawl-mcp-seo-audit
description: Audit a website's SEO with the self-hosted Firecrawl - metadata and heading review, site structure and internal links, keyword and SERP comparison, and prioritized fixes.
---

# SEO audit

Uses the tools and limits described in the `firecrawl-mcp` skill. Adapted from Firecrawl's `firecrawl-seo-audit` workflow (ISC).

Infer the site, target keywords, and output format from context. Ask at most 1-3 short questions only if blocked.

## Collect

1. Map the site to see URL structure. If map returns nothing (no sitemap), scrape the homepage with the `links` format and follow the main navigation.
2. Scrape key pages (homepage, product/service, pricing, blog, about, top landing pages). Metadata (title, description, canonical, og tags, language, robots) comes back in the `metadata` field with any format, so a small format is enough for it.
3. From each page note: title length, meta description, one H1, H2/H3 order, internal links, image alt text, thin or duplicate content. Terminal hosts: save `markdown` and `html` to files and search them. Chat hosts: HTML is very large - use a JSON scrape with a schema for headings, image alt texts, and internal link count, and read the markdown yourself only for the 2-3 most important pages.
4. If keywords are given, search each one and scrape the top 3 ranking pages for comparison.

Keep it to roughly 10-25 pages unless asked for more.

## Deliverable

```markdown
# SEO audit: [site]

## Summary
[Top risks and opportunities]

## Site structure
[URL patterns, sitemap presence, internal linking, orphan or broken pages]

## On-page findings
| Page | Title | Meta description | H1 | Issues |

## Keyword and competitor comparison
[Who ranks, what they do differently, content gaps]

## Prioritized fixes
[High / medium / low - exact change and the page it applies to]

## Sources
[Pages checked]
```

## Quality bar

- Specific fixes tied to a page, not generic advice.
- Separate measured facts (from metadata/HTML) from strategy opinions.
- Rankings vary by location and time; say that search results are a snapshot.
