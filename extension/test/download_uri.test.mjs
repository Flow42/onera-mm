import test from 'node:test';
import assert from 'node:assert/strict';

const sent = [];
globalThis.chrome = {
  runtime: {
    onMessage: { addListener() {} },
    async sendNativeMessage(_host, req) {
      sent.push(req);
      return { v: 1, id: req.id, status: 'ok', data: { accepted: true } };
    },
  },
};

const { handle } = await import('../src/service-worker.js');

test('download_uri validates scheme and forwards project_id', async () => {
  sent.length = 0;

  const bad = await handle({ action: 'download_uri', uri: 'javascript:alert(1)' });
  assert.equal(bad.ok, false);
  assert.equal(bad.code, 'malformed');
  assert.equal(sent.length, 0);

  const ok = await handle({
    action: 'download_uri',
    uri: 'https://example.com/archive.zip',
    projectId: 'proj-42',
    pageUrl: 'https://example.com/release',
    autoDeploy: true,
  });
  assert.equal(ok.ok, true);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].type, 'download_uri');
  assert.equal(sent[0].uri, 'https://example.com/archive.zip');
  assert.equal(sent[0].project_id, 'proj-42');
  assert.equal(sent[0].page_url, 'https://example.com/release');
  assert.equal(sent[0].auto_deploy, true);
});
