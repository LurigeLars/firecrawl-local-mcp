---
name: firecrawl-mcp-knowledge-base
description: Turn a documentation site, a site section, or a set of URLs into organized LLM-ready markdown with the self-hosted Firecrawl - reference docs, RAG chunks, or a docs mirror. Best in terminal hosts (Codex, Claude Code) that can write files.
---

# Knowledge base from the web

Uses the tools and limits described in the `firecrawl-mcp` skill. Adapted from Firecrawl's `firecrawl-knowledge-base` workflow (ISC).

Infer the source, the goal (reference, RAG, docs mirror), and the output folder. Ask at most 1-3 short questions only if blocked.

## Collect

1. Map the site to size it. If map is empty, scrape the docs index with the `links` format.
2. Choose the sections to include and a page limit; confirm with the user if it is over ~100 pages.
3. Terminal hosts: `firecrawl crawl URL --include-paths /docs --limit 50 --wait -o .firecrawl/<host>/crawl.json`, then write each page to `.firecrawl/<host>/<path>/index.md`. Chat hosts: use `firecrawl_map` (or a small crawl) to list the pages, then scrape each with the `summary` format and deliver summaries per page rather than full text; full text belongs in a terminal host.
4. Keep code blocks and tables intact; drop navigation and footers (`onlyMainContent`).

## Output modes

- **Reference:** markdown files, `index.md` table of contents, `sources.json` (url, title, fetched date).
- **RAG:** additionally split pages into chunks of about 500-1,000 words on heading boundaries, with a `manifest.json` (chunk id, source url, heading path).
- **Docs mirror:** full markdown tree with the table of contents.

Finish with a short report: what was collected, page and chunk counts, skipped or failed URLs, and how to refresh it.

## Quality bar

- Never mix content from different URLs in one file without attribution.
- Report failures instead of silently dropping pages.
