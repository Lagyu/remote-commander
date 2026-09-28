import test from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes, createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync, openSync, closeSync, ftruncateSync, readdirSync, writeSync, createReadStream } from 'node:fs';
import path from 'node:path';
import { harness, call, root } from './harness.mjs';
import { chromium } from 'playwright';

const MiB = 1024 * 1024, GiB = 1024 * MiB;
const hash = (data) => createHash('sha256').update(data).digest('hex');
async function post(url, data, offset = 0) {
  return fetch(`${url}/chunk?offset=${offset}`, { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: data, redirect: 'error' });
}
async function finish(url) { return fetch(`${url}/complete`, { method: 'POST', redirect: 'error' }); }
async function cancel(url) { assert.equal((await fetch(url, { method: 'DELETE' })).status, 200); }

test('binary transfer API: integrity, atomic commit, retries, HTTP Range, permissions and limits', { timeout: 150000 }, async (t) => {
  const h = await harness(); t.after(() => h.cleanup());
  await h.startWorker();
  const device = await h.pair('Transfer test'); await h.startAgent(device);
  const tokens = await h.authorize(); const client = await h.sdk(tokens);
  assert.match(client.getInstructions(), /download_file or upload_file/);
  assert.match(client.getInstructions(), /never embed file bytes in tool arguments/);
  const catalog = await client.listTools();
  assert.ok(catalog.tools.some(tool => tool.name === 'download_file'));
  assert.equal(catalog.tools.find(tool => tool.name === 'upload_file').inputSchema.properties.size.maximum, GiB);
  const args = { device_id: device.device_id };
  const config = await call(client, 'get_config', args);
  assert.equal(config.max_transfer_bytes, GiB); assert.equal(config.transfer_chunk_bytes, MiB);
  const bytes = randomBytes(2 * MiB + 37);
  const name = 'binary 日本語 test.bin';
  const upload = await call(client, 'upload_file', { ...args, path: name, size: bytes.length, sha256: hash(bytes) });
  const url = upload.upload_url;
  assert.equal(upload.expires_in, 3600); assert.equal(upload.chunk_bytes, MiB);
  const htmlPage = await fetch(url); assert.equal(htmlPage.status, 200); assert.match(await htmlPage.text(), /type="file"/);
  assert.equal((await fetch(new URL('/transfer.js',url))).status,200);
  assert.equal((await fetch(new URL('/transfer.css',url))).status,200);
  assert.equal((await fetch(url, { headers: { Origin: 'https://untrusted.example' } })).status,403);
  assert.equal((await fetch(url.replace('/upload/','/download/'))).status,404);
  assert.equal((await post(url,bytes.subarray(0,1),1)).status,409);
  assert.equal((await post(url,Buffer.alloc(MiB+1))).status,413);
  assert.equal((await post(url,Buffer.from([1]),bytes.length)).status,400);
  assert.equal((await finish(url)).status,409);
  assert.equal((await post(url,bytes.subarray(0,MiB))).status,200);
  assert.equal((await post(url,bytes.subarray(0,MiB))).status,200);
  assert.equal((await post(url,Buffer.from([bytes[0]^255]))).status,409);
  assert.equal((await (await fetch(`${url}/status`)).json()).bytes_received,MiB);
  assert.equal(existsSync(path.join(h.files,name)),false);
  for(let offset=MiB;offset<bytes.length;offset+=MiB) assert.equal((await post(url,bytes.subarray(offset,offset+MiB),offset)).status,200);
  const result=await (await finish(url)).json();
  assert.equal(result.complete,true); assert.equal(result.sha256,hash(bytes));
  assert.equal((await finish(url)).status,200);
  assert.deepEqual(readFileSync(path.join(h.files,name)),bytes);
  assert.equal((await call(client,'upload_file',{...args,path:name,size:0},true)).error,'operation_failed');
  await cancel(url);
  assert.equal((await fetch(`${url}/status`)).status,404);
  assert.deepEqual(readFileSync(path.join(h.files,name)),bytes);

  const download=await call(client,'download_file',{...args,path:name});
  const response=await fetch(download.download_url);
  assert.equal(response.status,200); assert.equal(response.headers.get('content-length'),String(bytes.length));
  assert.equal(response.headers.get('accept-ranges'),'bytes');
  assert.match(response.headers.get('content-disposition'),/filename\*=UTF-8''/);
  assert.deepEqual(Buffer.from(await response.arrayBuffer()),bytes);
  const range=await fetch(download.download_url,{headers:{Range:'bytes=1048570-1048590'}});
  assert.equal(range.status,206); assert.equal(range.headers.get('content-range'),`bytes 1048570-1048590/${bytes.length}`);
  assert.deepEqual(Buffer.from(await range.arrayBuffer()),bytes.subarray(1048570,1048591));
  const suffix=await fetch(download.download_url,{headers:{Range:'bytes=-7'}});
  assert.equal(suffix.status,206); assert.deepEqual(Buffer.from(await suffix.arrayBuffer()),bytes.subarray(-7));
  const resume=await fetch(download.download_url,{headers:{Range:`bytes=${bytes.length-5}-`}});
  assert.equal(resume.status,206); assert.deepEqual(Buffer.from(await resume.arrayBuffer()),bytes.subarray(-5));
  for(const value of ['bytes=-0','bytes=100-20',`bytes=${bytes.length}-`,'bytes=0-1,4-6']) {
    const invalid=await fetch(download.download_url,{headers:{Range:value}});
    assert.equal(invalid.status,416); assert.equal(invalid.headers.get('content-range'),`bytes */${bytes.length}`);
  }
  const head=await fetch(download.download_url,{method:'HEAD'}); assert.equal(head.status,200); assert.equal(head.headers.get('content-length'),String(bytes.length)); assert.equal((await head.arrayBuffer()).byteLength,0);
  const fallback=await fetch(download.download_url,{headers:{Range:'bytes=0-1','If-Range':'"different"'}});
  assert.equal(fallback.status,200); await fallback.arrayBuffer();
  await cancel(download.download_url);

  // Exact-boundary sparse file verifies the last byte without allocating 1 GiB in RAM.
  const fd=openSync(path.join(h.files,'boundary.bin'),'w'); ftruncateSync(fd,GiB); closeSync(fd);
  const boundary=await call(client,'download_file',{...args,path:'boundary.bin'});
  assert.equal(boundary.bytes,GiB);
  const last=await fetch(boundary.download_url,{headers:{Range:`bytes=${GiB-1}-`}});
  assert.equal(last.status,206); assert.deepEqual(Buffer.from(await last.arrayBuffer()),Buffer.from([0]));
  await cancel(boundary.download_url);
  const fd2=openSync(path.join(h.files,'boundary.bin'),'r+'); ftruncateSync(fd2,GiB+1); closeSync(fd2);
  assert.equal((await call(client,'download_file',{...args,path:'boundary.bin'},true)).error,'operation_failed');
  await assert.rejects(()=>client.callTool({name:'upload_file',arguments:{...args,path:'too-large.bin',size:GiB+1}}));
  const full=await call(client,'upload_file',{...args,path:'full.bin',size:GiB}); assert.equal(full.bytes,GiB); await cancel(full.upload_url);
  const empty=await call(client,'upload_file',{...args,path:'empty.bin',size:0});
  assert.equal((await finish(empty.upload_url)).status,200); await cancel(empty.upload_url);
  const emptyDownload=await call(client,'download_file',{...args,path:'empty.bin'});
  const emptyResponse=await fetch(emptyDownload.download_url); assert.equal(emptyResponse.status,200); assert.equal((await emptyResponse.arrayBuffer()).byteLength,0); await cancel(emptyDownload.download_url);
  const mismatch=await call(client,'upload_file',{...args,path:'bad-hash.bin',size:1,sha256:'0'.repeat(64)});
  assert.equal((await post(mismatch.upload_url,Buffer.from([1]))).status,200); assert.equal((await finish(mismatch.upload_url)).status,409); assert.equal(existsSync(path.join(h.files,'bad-hash.bin')),false); await cancel(mismatch.upload_url);
  const changed=await call(client,'download_file',{...args,path:name}); writeFileSync(path.join(h.files,name),'changed');
  assert.equal((await fetch(changed.download_url)).status,409); await cancel(changed.download_url);
  assert.equal((await call(client,'upload_file',{...args,path:'../outside.txt',size:0},true)).error,'operation_failed');
  const readonly=await h.sdk(await h.authorize('commander:read'));
  assert.equal((await call(readonly,'upload_file',{...args,path:'no.txt',size:0},true)).error,'insufficient_scope');
  const device2=await h.pair('Read-only transfers'); await h.startAgent(device2,[]);
  assert.equal((await call(client,'upload_file',{device_id:device2.device_id,path:'no.txt',size:0},true)).error,'operation_failed');
  const browserUpload=await call(client,'upload_file',{...args,path:'browser.bin',size:8});
  const browser=await chromium.launch({headless:true}); h.browsers.push(browser);
  const page=await browser.newPage(); const errors=[]; page.on('pageerror',error=>errors.push(error.message));
  let lostReplies=0;
  await page.route(`${browserUpload.upload_url}/complete`, async route => {
    const response=await route.fetch(); assert.equal(response.status(),200);
    lostReplies++; await route.abort('failed');
  });
  await page.goto(browserUpload.upload_url);
  await page.locator('#file').setInputFiles({name:'browser.bin',mimeType:'application/octet-stream',buffer:Buffer.from([0,255,128,1,2,3,4,5])});
  await page.locator('#start').click();
  await page.waitForFunction(()=>document.getElementById('status').textContent.startsWith('Upload complete.'),{},{timeout:30000});
  assert.deepEqual(readFileSync(path.join(h.files,'browser.bin')),Buffer.from([0,255,128,1,2,3,4,5]));
  assert.ok(lostReplies >= 4, 'browser recovered after every completion reply was lost');
  assert.deepEqual(errors,[]);
  await cancel(browserUpload.upload_url);
  assert.equal(readdirSync(h.files).some(name=>name.startsWith('.rdc-upload-')),false);
});

const largeOptions = { skip: process.env.RDC_TEST_GIB !== '1', timeout: 45 * 60 * 1000 };
async function largeHarness(t) {
  const h=await harness({agentBinary:path.join(root, 'target/release/remote-commander')});
  t.after(()=>h.cleanup()); await h.startWorker();
  const device=await h.pair('1 GiB verification'); await h.startAgent(device);
  const client=await h.sdk(await h.authorize());
  return {h,device,client};
}
function expectedHash(block) {
  const expected=createHash('sha256');
  for(let i=0;i<1024;i++) expected.update(block);
  return expected.digest('hex');
}
async function reliableChunk(url,block,offset) {
  let last;
  for(let attempt=0;attempt<4;attempt++) {
    let response;
    try { response=await post(url,block,offset); }
    catch(error) { last=error; }
    if(response) {
      const text=await response.text();
      if(response.status===200) return;
      assert.ok([429,502,503,504].includes(response.status), `upload offset ${offset}: HTTP ${response.status}: ${text}`);
      last=new Error(`upload offset ${offset}: HTTP ${response.status}: ${text}`);
    }
    await new Promise(resolve=>setTimeout(resolve,500*2**attempt));
  }
  throw last;
}

test('full 1 GiB upload with matching server and on-disk SHA-256', largeOptions, async (t) => {
  const {h,device,client}=await largeHarness(t);
  const block=randomBytes(MiB); const digest=expectedHash(block);
  const upload=await call(client,'upload_file',{device_id:device.device_id,path:'one-gib.bin',size:GiB,sha256:digest});
  for(let offset=0;offset<GiB;offset+=MiB) {
    await reliableChunk(upload.upload_url,block,offset);
    if(offset%(128*MiB)===0) console.log(`1 GiB upload verified through ${offset/MiB+1} MiB`);
  }
  const completed=await finish(upload.upload_url); assert.equal(completed.status,200);
  const result=await completed.json(); assert.equal(result.sha256,digest);
  const onDisk=createHash('sha256'); let size=0;
  for await(const chunk of createReadStream(path.join(h.files,'one-gib.bin'))) { size+=chunk.length; onDisk.update(chunk); }
  assert.equal(size,GiB); assert.equal(onDisk.digest('hex'),digest);
  await cancel(upload.upload_url);
  console.log(`Verified complete ${GiB}-byte upload against on-disk bytes; SHA-256 ${digest}`);
});

test('full 1 GiB streamed download with matching SHA-256', largeOptions, async (t) => {
  const {h,device,client}=await largeHarness(t);
  const block=randomBytes(MiB); const digest=expectedHash(block);
  const fd=openSync(path.join(h.files,'one-gib.bin'),'wx',0o600);
  try { for(let i=0;i<1024;i++) { let offset=0; while(offset<block.length) offset+=writeSync(fd,block,offset,block.length-offset); } }
  finally { closeSync(fd); }
  const download=await call(client,'download_file',{device_id:device.device_id,path:'one-gib.bin'});
  const response=await fetch(download.download_url); assert.equal(response.status,200);
  assert.equal(response.headers.get('content-length'),String(GiB));
  const received=createHash('sha256'); let size=0, logged=0;
  for await(const chunk of response.body){
    size+=chunk.byteLength; received.update(chunk);
    if(size-logged>=128*MiB) { logged=size; console.log(`1 GiB download verified through ${Math.floor(size/MiB)} MiB`); }
  }
  assert.equal(size,GiB); assert.equal(received.digest('hex'),digest); await cancel(download.download_url);
  console.log(`Verified complete ${GiB}-byte streamed download; SHA-256 ${digest}`);
});
