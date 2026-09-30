---
name: firecrawl-mcp-design-extract
description: Extract a website's design system (colors, typography, spacing, radii, components, layout patterns) into an agent-ready DESIGN.md using the self-hosted Firecrawl, from the page HTML and its CSS files.
---

# Design system extraction

Uses the tools and limits described in the `firecrawl-mcp` skill. Adapted from Firecrawl's `firecrawl-website-design-clone` workflow (ISC).

The hosted `branding` and `screenshot` formats are not available here. Work from the markup and stylesheets instead, and say that no screenshot was captured.

Raw HTML and CSS are large. In terminal hosts save them to files and search them for the tokens below. In chat hosts, ask the local model instead of pulling the files into the chat: scrape each CSS file with the `query` format (for example "list the CSS custom properties, the most used colors, and the font families"), and read raw HTML yourself only for the page structure.

## Collect

1. Scrape the target page with formats `rawHtml` and `markdown`. Terminal hosts: `firecrawl scrape URL --format rawHtml -o .firecrawl/design/page.html`.
2. Find `<link rel="stylesheet" href=...>` and inline `<style>` blocks. Resolve relative hrefs against the page URL and scrape each first-party CSS file with `rawHtml` (skip third-party widget CSS).
3. From CSS collect: custom properties (`--*`), color values by frequency, `font-family`, `@font-face`, font sizes and weights, spacing scale, border radius, shadows, breakpoints (`@media`), transitions.
4. From HTML collect: page sections in order, navigation, hero, cards, buttons, forms, footer, and the class names that style them.
5. Optionally repeat for one or two more representative pages (pricing, docs).

## Deliverable: DESIGN.md

```markdown
# Design system: [site] (source: [url], extracted [date])

## Overview        [visual style in 2-3 sentences]
## Colors          | Token/usage | Value | Evidence (selector or variable) |
## Typography      [families with fallbacks, scale, weights, line heights]
## Spacing and layout [scale, container widths, grid, breakpoints]
## Shape and depth [radii, borders, shadows]
## Components      [button, card, nav, form - states when visible in CSS]
## Motion          [transitions or "none observed"]
## Build instructions for an agent [how to recreate with CSS variables]
## Gaps            [what could not be determined, e.g. no screenshot]
```

## Quality bar

- Every token comes from observed CSS or HTML; mark guesses as guesses.
- Do not copy logos, images, or text content as your own; describe the style only.
