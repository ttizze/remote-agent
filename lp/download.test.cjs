const { test } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(__dirname + '/public/download.js', 'utf8');
const download = 'https://github.com/ttizze/remote-agent/releases/download/prd-75/bex-windows-x86_64-release.zip';
const preview = 'https://github.com/TooriG/remote-agent/releases/download/windows-preview-20261015-1/bex-windows-x86_64-dev.zip';

async function renderWindows({ language = 'en', platform = 'Win32', userAgent, assets = [], ok = true, fail = false } = {}) {
  const element = () => ({ classList: { toggle() {} }, innerHTML: '', href: '' });
  const primary = [element(), element()];
  const grid = [element()];
  const menu = [element()];
  const mobile = ['android', 'ios'].map(id => ({ ...element(), dataset: { dlMob: id } }));
  let isMobile = false;
  const context = {
    window: {}, URL, AbortSignal,
    navigator: { platform, userAgent: userAgent || (platform === 'Win32' ? 'Windows NT 10.0' : 'Macintosh') },
    document: {
      documentElement: { lang: language, classList: { toggle(name, value) { if (name === 'mob') isMobile = value; } } },
      querySelectorAll(selector) {
        return ({ '[data-dl-primary]': primary, '[data-dl-grid]': grid, '[data-dl-menu]': menu, '[data-dl-mob]': mobile })[selector] || [];
      },
      addEventListener() {}
    },
    MutationObserver: class { observe() {} },
    fetch: async () => {
      if (fail) throw new Error('offline');
      return { ok, json: async () => ({ assets }) };
    }
  };
  vm.runInNewContext(source, context);
  await new Promise(resolve => setImmediate(resolve));
  return { primary, grid: grid[0].innerHTML, menu: menu[0].innerHTML, mobile, isMobile };
}

for (const language of ['ja', 'en']) {
  test(`Android phone ${language} prominently links to the published preview APK`, async () => {
    const apk = 'https://github.com/TooriG/remote-agent/releases/download/android-preview-16/bex-android-preview.apk';
    const result = await renderWindows({ language, platform: 'Linux arm64', userAgent: 'Android 17 Mobile', assets: [{ name: 'bex-android-preview.apk', state: 'uploaded', browser_download_url: apk }] });
    assert.equal(result.isMobile, true);
    assert.equal(result.primary[0].href, apk);
    assert.equal(result.mobile[0].href, apk);
    assert.match(result.mobile[0].innerHTML, language === 'ja' ? /Androidで使う/ : /Use on Android/);
    assert.ok(result.grid.includes(`href="${apk}"`));
  });
}

test('Android download remains direct when the upstream release API is offline', async () => {
  const result = await renderWindows({ platform: 'Linux arm64', userAgent: 'Android 17 Mobile', fail: true });
  assert.equal(result.isMobile, true);
  assert.match(result.mobile[0].innerHTML, /Use on Android/);
  assert.equal(result.mobile[0].href, 'https://github.com/TooriG/remote-agent/releases/download/android-preview-16/bex-android-preview.apk');
  assert.ok(!result.mobile[0].innerHTML.includes('coming soon'));
});

for (const language of ['en', 'ja']) {
  test(`Windows ${language} hero, menu and card download the uploaded ZIP directly`, async () => {
    const result = await renderWindows({ language, assets: [{ name: 'bex-windows-x86_64-release.zip', state: 'uploaded', browser_download_url: download }] });
    for (const button of result.primary) {
      assert.equal(button.href, download);
      assert.match(button.innerHTML, language === 'ja' ? /Windows版をダウンロード/ : /Download for Windows/);
    }
    assert.ok(result.grid.includes(`href="${download}"`));
    const mac = await renderWindows({ language, platform: 'MacIntel', assets: [{ name: 'bex-windows-x86_64-release.zip', state: 'uploaded', browser_download_url: download }] });
    assert.ok(mac.menu.includes(`href="${download}"`));
    assert.ok(!result.primary[0].innerHTML.includes('coming soon'));
  });
}
for (const [name, options] of Object.entries({
  'Mac-only release': {},
  'failed API': { ok: false },
  offline: { fail: true },
  'unfinished upload': { assets: [{ name: 'bex-windows-x86_64-release.zip', state: 'new', browser_download_url: download }] },
  'foreign download': { assets: [{ name: 'bex-windows-x86_64-release.zip', state: 'uploaded', browser_download_url: 'https://example.com/bex.zip' }] }
})) {
  test(`${name} retains the pinned Windows ZIP download`, async () => {
    const result = await renderWindows(options);
    assert.equal(result.primary[0].href, preview);
    assert.match(result.primary[0].innerHTML, /Download for Windows/);
    assert.ok(result.grid.includes(`href="${preview}"`));
    assert.match(result.grid, /preview ZIP/);
    const windowsCard = result.grid.match(/<h3>Windows<\/h3>[\s\S]*?<\/a>/)[0];
    assert.doesNotMatch(windowsCard, /Install from source|docs\/WINDOWS/);
    assert.ok(!result.primary[0].innerHTML.includes('coming soon'));
  });
}

for (const language of ['ja', 'en']) {
  test(`Windows ${language} menu also retains the preview without the release API`, async () => {
    const result = await renderWindows({ language, platform:'MacIntel', fail:true });
    assert.ok(result.menu.includes(`href="${preview}"`));
  });
}
