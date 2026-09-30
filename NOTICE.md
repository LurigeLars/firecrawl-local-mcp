# Third-party notices and provenance

This repository is a deployment/integration wrapper around upstream Firecrawl. It is not an official Firecrawl project and is not a GitHub fork of `firecrawl/firecrawl`.

## Firecrawl core

The runtime source checkout is obtained separately from:
https://github.com/firecrawl/firecrawl

Firecrawl declares the AGPL-3.0 license. A license reference for the upstream AGPL-3.0 terms is included at `LICENSES/FIRECRAWL-AGPL-3.0.txt`. The upstream Firecrawl source tree is not vendored in this repository.

## Firecrawl MCP server

The deployment installs/uses the official Firecrawl MCP server from:
https://github.com/firecrawl/firecrawl-mcp-server

Its upstream MIT license is included at `LICENSES/FIRECRAWL-MCP-MIT.txt`.

## Firecrawl workflow skills

Some files under `marketplace/plugins/firecrawl-mcp/skills/` are adapted from Firecrawl workflow skills. Their ISC notice is preserved in `marketplace/plugins/firecrawl-mcp/THIRD_PARTY_NOTICES.md` and copied to `LICENSES/FIRECRAWL-SKILLS-ISC.txt`.

## Local wrapper code

The remaining deployment, gateway, proxy, test, and configuration code is local wrapper code licensed under the repository's root MIT `LICENSE`, unless a file explicitly states otherwise. Third-party components and adapted material remain subject to their upstream licenses described above.
