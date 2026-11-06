const { test } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(__dirname + '/public/download.generated.js', 'utf8');
const download = 'https://github.com/ttizze/remote-agent/releases/download/prd-75/bex-windows-x86_64-setup.exe';
const pinnedWindows = 'https://github.com/ttizze/remote-agent/releases/download/prd-133/bex-windows-x86_64-setup.exe';

async function renderDownloads({ language = 'en', platform = 'Win32', userAgent, assets = [], release = { tag_name: 'prd-75', assets }, ok = true, fail = false } = {}) {
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
    fetch: async url => {
      assert.equal(url, 'https://api.github.com/repos/ttizze/remote-agent/releases/latest');
      if (fail) throw new Error('offline');
      return { ok, json: async () => release };
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
    assert.equal(result.primary[0].href, pinnedWindows);
    assert.match(result.primary[0].innerHTML, /Download for Windows/);
    assert.ok(result.grid.includes(`href="${pinnedWindows}"`));
    assert.match(result.grid, /EXE/);
    const windowsCard = result.grid.match(/<h3>Windows<\/h3>[\s\S]*?<\/a>/)[0];
    assert.doesNotMatch(windowsCard, /Install from source|docs\/WINDOWS/);
    assert.ok(!result.primary[0].innerHTML.includes('coming soon'));
  });
}

for (const language of ['ja', 'en']) {
  test(`Windows ${language} menu also retains the published installer without the release API`, async () => {
    const result = await renderDownloads({ language, platform:'MacIntel', fail:true });
    assert.ok(result.menu.includes(`href="${pinnedWindows}"`));
  });
}

test('the latest production release supplies the same packages for every platform', async () => {
  const prefix = 'https://github.com/ttizze/remote-agent/releases/download/prd-145/';
  const assets = ['bex-macOS-arm64.zip', 'bex-windows-x86_64-setup.exe', 'bex-android.apk', 'bex-linux-x86_64.tar.gz', 'release.json']
    .map(name => ({ name, state: 'uploaded', browser_download_url: prefix + name }));
  const result = await renderDownloads({ assets });
  for (const asset of assets.filter(asset => asset.name !== 'release.json')) assert.ok(result.grid.includes(`href="${asset.browser_download_url}"`));
  assert.ok(result.grid.includes('https://apps.apple.com/app/id6816015343'));
  assert.doesNotMatch(result.grid, /testflight.apple.com/);
});

for (const release of [
  { tag_name: 'nightly-999', prerelease: true },
  { tag_name: 'prd-999', draft: true },
  { tag_name: 'android-preview-999' },
]) {
  test(`${release.tag_name} cannot change the public production download`, async () => {
    release.assets = [{ name: 'bex-windows-x86_64-setup.exe', state: 'uploaded', browser_download_url: download }];
    assert.equal((await renderDownloads({ release })).primary[0].href, pinnedWindows);
  });
}
