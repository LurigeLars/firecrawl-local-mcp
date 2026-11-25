// Existing Windows registrations remain valid across the explicit switch.
import {existsSync} from 'node:fs';
import {join} from 'node:path';
export function shouldDelegateRustClient(env, platform=process.platform, exists=existsSync) {
  return platform==='win32' && !!env.LOCALAPPDATA &&
    (env.FIRECRAWL_API_URL??'http://127.0.0.1:3002')==='http://127.0.0.1:3002' &&
    env.FIRECRAWL_API_KEY===undefined && env.FIRECRAWL_API_KEY_FILE===undefined &&
    exists(join(env.LOCALAPPDATA,'FirecrawlLocal','rust-transition.lock'));
}
