// Loaded before the actual locked MCP CLI, without a shell or child wrapper.
import { candidateApiEnvironment } from './mcp-api-env.mjs';

if (process.env.FIRECRAWL_API_KEY_FILE !== undefined) {
  const env = candidateApiEnvironment(process.env, 'http://api:3002', 'http://api:3002');
  process.env.FIRECRAWL_API_URL = env.FIRECRAWL_API_URL;
  process.env.FIRECRAWL_API_KEY = env.FIRECRAWL_API_KEY;
  delete process.env.FIRECRAWL_API_KEY_FILE;
}
