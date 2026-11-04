const { test } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(__dirname + '/public/download.js', 'utf8');
const download = 'https://github.com/ttizze/remote-agent/releases/download/prd-75/bex-windows-x86_64-setup.exe';
const preview = 'https://github.com/TooriG/remote-agent/releases/download/windows-preview-20261020-storage-notice/bex-windows-x86_64-setup.exe';

async function renderDownloads({ language = 'en', platform = 'Win32', userAgent, assets = [], releases = [{ assets }], ok = true, fail = false } = {}) {
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
      return { ok, json: async () => releases };
    }
  };
  vm.runInNewContext(source, context);
  await new Promise(resolve => setImmediate(resolve));
  return { primary, grid: grid[0].innerHTML, menu: menu[0].innerHTML, mobile, isMobile };
}

for (const language of ['ja', 'en']) {
  test(`Android phone ${language} prominently links to the published preview APK`, async () => {
    const apk = 'https://github.com/TooriG/remote-agent/releases/download/android-preview-79/bex-android-preview.apk';
    const result = await renderDownloads({ language, platform: 'Linux arm64', userAgent: 'Android 17 Mobile', assets: [{ name: 'bex-android-preview.apk', state: 'uploaded', browser_download_url: apk }] });
    assert.equal(result.isMobile, true);
    assert.equal(result.primary[0].href, apk);
    assert.equal(result.mobile[0].href, apk);
    assert.match(result.mobile[0].innerHTML, language === 'ja' ? /Androidで使う/ : /Use on Android/);
    assert.ok(result.grid.includes(`href="${apk}"`));
  });
}

test('Android download remains direct when the upstream release API is offline', async () => {
  const result = await renderDownloads({ platform: 'Linux arm64', userAgent: 'Android 17 Mobile', fail: true });
  assert.equal(result.isMobile, true);
  assert.match(result.mobile[0].innerHTML, /Use on Android/);
  assert.equal(result.mobile[0].href, 'https://github.com/TooriG/remote-agent/releases/download/android-preview-79/bex-android-preview.apk');
  assert.ok(!result.mobile[0].innerHTML.includes('coming soon'));
});

for (const language of ['en', 'ja']) {
  test(`Windows ${language} hero, menu and card download the uploaded EXE directly`, async () => {
    const result = await renderDownloads({ language, assets: [{ name: 'bex-windows-x86_64-setup.exe', state: 'uploaded', browser_download_url: download }] });
    for (const button of result.primary) {
      assert.equal(button.href, download);
      assert.match(button.innerHTML, language === 'ja' ? /Windows版をダウンロード/ : /Download for Windows/);
    }
    assert.ok(result.grid.includes(`href="${download}"`));
    const mac = await renderDownloads({ language, platform: 'MacIntel', assets: [{ name: 'bex-windows-x86_64-setup.exe', state: 'uploaded', browser_download_url: download }] });
    assert.ok(mac.menu.includes(`href="${download}"`));
    assert.ok(!result.primary[0].innerHTML.includes('coming soon'));
  });
}
for (const [name, options] of Object.entries({
  'Mac-only release': {},
  'failed API': { ok: false },
  offline: { fail: true },
  'old ZIP-only release': { assets: [{ name: 'bex-windows-x86_64-release.zip', state: 'uploaded', browser_download_url: 'https://github.com/ttizze/remote-agent/releases/download/prd-75/bex-windows-x86_64-release.zip' }] },
  'unfinished upload': { assets: [{ name: 'bex-windows-x86_64-setup.exe', state: 'new', browser_download_url: download }] },
  'foreign download': { assets: [{ name: 'bex-windows-x86_64-setup.exe', state: 'uploaded', browser_download_url: 'https://example.com/bex.zip' }] }
})) {
  test(`${name} retains the pinned Windows EXE download`, async () => {
    const result = await renderDownloads(options);
    assert.equal(result.primary[0].href, preview);
    assert.match(result.primary[0].innerHTML, /Download for Windows/);
    assert.ok(result.grid.includes(`href="${preview}"`));
    assert.match(result.grid, /preview EXE/);
    const windowsCard = result.grid.match(/<h3>Windows<\/h3>[\s\S]*?<\/a>/)[0];
    assert.doesNotMatch(windowsCard, /Install from source|docs\/WINDOWS/);
    assert.ok(!result.primary[0].innerHTML.includes('coming soon'));
  });
}

for (const language of ['ja', 'en']) {
  test(`Windows ${language} menu also retains the preview without the release API`, async () => {
    const result = await renderDownloads({ language, platform:'MacIntel', fail:true });
    assert.ok(result.menu.includes(`href="${preview}"`));
  });
}

test('Mac and Windows select the newest uploaded asset for each platform across stable releases', async () => {
  const mac = 'https://github.com/ttizze/remote-agent/releases/download/prd-77/bex-macOS-arm64.zip';
  const asset = (name, url, state = 'uploaded') => ({ name, state, browser_download_url: url });
  const releases = [
    { draft: true, assets: [asset('bex-macOS-arm64.zip', mac + '-draft')] },
    { prerelease: true, assets: [asset('bex-macOS-arm64.zip', mac + '-prerelease')] },
    { assets: [asset('bex-macOS-arm64.zip', mac + '-pending', 'new')] },
    { assets: [asset('bex-macOS-arm64.zip', 'https://example.test/bex.zip')] },
    { assets: [asset('bex-macOS-arm64.zip', mac)] },
    { assets: [asset('bex-75-macOS-arm64.zip', mac + '-old'), asset('bex-windows-x86_64-setup.exe', download)] }
  ];
  const result = await renderDownloads({ platform: 'MacIntel', releases });
  assert.equal(result.primary[0].href, mac);
  assert.ok(result.menu.includes(`href="${download}"`));
  assert.ok(result.grid.includes(`href="${mac}"`));
  assert.doesNotMatch(result.grid, /34MB/);
});

test('Mac retains its published download offline and accepts the historical versioned ZIP name', async () => {
  const pinned = 'https://github.com/ttizze/remote-agent/releases/download/prd-74/bex-74-macOS-arm64.zip';
  assert.equal((await renderDownloads({ platform: 'MacIntel', fail: true })).primary[0].href, pinned);
  const mac = pinned.replaceAll('74', '76');
  const result = await renderDownloads({ platform: 'MacIntel', assets: [{ name: 'bex-76-macOS-arm64.zip', state: 'uploaded', browser_download_url: mac }] });
  assert.equal(result.primary[0].href, mac);
});
