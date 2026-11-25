import {test} from 'node:test';
import assert from 'node:assert/strict';
import {shouldDelegateRustClient} from './rust-client-mode.mjs';
const env={LOCALAPPDATA:'synthetic-local-state'};
test('only local keyless Windows clients delegate with a transition marker',()=>{
  let checked=0;
  const marker=p=>{checked++;assert.match(p,/FirecrawlLocal[\\/]rust-transition\.lock$/);return true;};
  assert.equal(shouldDelegateRustClient(env,'win32',marker),true);
  assert.equal(shouldDelegateRustClient({...env,FIRECRAWL_API_URL:'http://127.0.0.1:3002'},'win32',marker),true);
  assert.equal(shouldDelegateRustClient(env,'win32',()=>false),false);
  for(const e of [{},{...env,FIRECRAWL_API_URL:'https://example.invalid'},
    {...env,FIRECRAWL_API_URL:'http://127.0.0.1:3002/'},
    {...env,FIRECRAWL_API_KEY:'synthetic-key'}, {...env,FIRECRAWL_API_KEY_FILE:'synthetic-file'},
    {...env,FIRECRAWL_API_KEY:''}]){
    assert.equal(shouldDelegateRustClient(e,'win32',()=>{throw Error('unexpected marker lookup');}),false);
  }
  assert.equal(shouldDelegateRustClient(env,'linux',()=>true),false);
  assert.equal(checked,2);
});
