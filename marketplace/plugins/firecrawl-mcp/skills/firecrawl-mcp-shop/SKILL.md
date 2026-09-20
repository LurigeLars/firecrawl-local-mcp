---
name: firecrawl-mcp-shop
description: Research a product purchase with the self-hosted Firecrawl - compare models, specs, prices, and reviews across retailers and review sites, and recommend what to buy. Research only; never adds to cart or checks out.
---

# Product research

Uses the tools and limits described in the `firecrawl-mcp` skill. Adapted from Firecrawl's `firecrawl-shop` workflow (ISC).

Infer the product need, budget, country or currency, and must-haves. Ask at most 1-3 short questions only if blocked (budget and country usually matter most).

## Collect

1. Search for candidate products and for trusted reviews (independent test sites, then forums such as Reddit for long-term issues).
2. Scrape the official product pages for specs, and 2-3 retailer pages per shortlisted product for price and availability in the user's country (for Sweden: e.g. Prisjakt, retailers' own sites).
3. Scrape the key reviews; note verdicts and recurring complaints.

Cart actions, logins, and checkout are not possible here and are out of scope.

## Deliverable

```markdown
# [Product need] - recommendation

## Pick
[Product, why, best price found with retailer and date]

## Comparison
| Product | Key specs | Price (currency) | Where | Review verdict |

## Watch out for
[Common complaints, compatibility, warranty]

## Sources
```

## Quality bar

- Prices as shown with currency and the date checked; they change quickly.
- Separate review consensus from single opinions.
- Disclose when a source is affiliate-driven or when evidence is thin.
