// Platform-aware download buttons. Edit DL to change files and links.
(() => {
  const REL = 'https://github.com/ttizze/remote-agent/releases/tag/prd-74';
  const SRC = 'https://github.com/ttizze/remote-agent/blob/main/README.md';
  const DL = window.BEX_DL = {
    mac:     { name: 'macOS',         file: { ja: 'Apple Silicon · macOS 26 · .zip 34MB', en: 'Apple Silicon · macOS 26 · .zip 34MB' }, url: 'https://github.com/ttizze/remote-agent/releases/download/prd-74/bex-74-macOS-arm64.zip', ready: true },
    ios:     { name: 'iPhone / iPad', file: { ja: 'iOS 26 · TestFlightで配布中', en: 'iOS 26 · via TestFlight' }, url: 'https://testflight.apple.com/join/UJbK9Ww7', ready: true, cta: { ja: 'TestFlightで入手', en: 'Get on TestFlight' } },
    win:     { name: 'Windows',       file: { ja: 'Windows 11 · x86-64 · ZIP（プレビュー）', en: 'Windows 11 · x86-64 · preview ZIP' }, url: 'https://github.com/TooriG/remote-agent/releases/download/windows-preview-20261015-1/bex-windows-x86_64-dev.zip', ready: true },
    android: { name: 'Android',       file: { ja: 'Android 8.0以降 · 64-bit · APK（プレビュー）', en: 'Android 8.0+ · 64-bit · preview APK' }, url: 'https://github.com/TooriG/remote-agent/releases/download/android-preview-16/bex-android-preview.apk', ready: true, cta: { ja: 'Androidで使う', en: 'Use on Android' } },
    linux:   { name: 'Linux',         file: { ja: 'ヘッドレスHost・CLI', en: 'Headless Host and CLI' }, url: SRC + '#headless-linux-and-iphone-pairing', ready: true, source: true }
  };
  const ICON = {
    mac: '<path d="M16.4 12.6c0-2.4 2-3.6 2.1-3.7-1.2-1.7-3-1.9-3.6-2-1.5-.2-3 .9-3.8.9-.8 0-2-.9-3.3-.9-1.7 0-3.3 1-4.1 2.5-1.8 3.1-.5 7.6 1.3 10.1.8 1.2 1.8 2.6 3.1 2.5 1.3 0 1.7-.8 3.3-.8 1.5 0 2 .8 3.3.8 1.4 0 2.2-1.2 3-2.4.9-1.4 1.3-2.8 1.3-2.8s-2.6-1-2.6-4.2zM14 5.2c.7-.8 1.1-1.9 1-3-1 0-2.1.7-2.8 1.5-.6.7-1.2 1.8-1 2.9 1 .1 2.1-.6 2.8-1.4z"/>',
    win: '<path d="M3 5.5 10 4.5v7H3zM11 4.3 21 3v8.5H11zM3 12.5h7v7l-7-1zM11 12.5h10V21l-10-1.4z"/>',
    ios: '<rect x="6" y="2.5" width="12" height="19" rx="2.6" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M10.5 18.5h3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>',
    android: '<path d="M6 10h12v7.5a1.5 1.5 0 0 1-1.5 1.5H15v2.5a1.25 1.25 0 0 1-2.5 0V19h-1v2.5a1.25 1.25 0 0 1-2.5 0V19H7.5A1.5 1.5 0 0 1 6 17.5zM3.8 10.2a1.2 1.2 0 0 1 2.4 0v5a1.2 1.2 0 0 1-2.4 0zM17.8 10.2a1.2 1.2 0 0 1 2.4 0v5a1.2 1.2 0 0 1-2.4 0zM6.2 9a5.8 5.8 0 0 1 11.6 0zM9.5 6.6a.7.7 0 1 0 0-.01zM14.5 6.6a.7.7 0 1 0 0-.01zM8 3l1.3 2M16 3l-1.3 2" stroke="currentColor" stroke-width="1" />',
    linux: '<path d="M12 2.5c-2.2 0-3.4 1.9-3.4 4.4 0 1.5.3 2.4-.8 3.9-1.4 1.9-2.8 4.2-2.3 6.7.2 1 .9 1.6 1.6 1.8-.3.7-.2 1.6.7 2 1 .4 2.2-.2 3.1-.4.6-.1 1.6-.1 2.2 0 .9.2 2.1.8 3.1.4.9-.4 1-1.3.7-2 .7-.2 1.4-.8 1.6-1.8.5-2.5-.9-4.8-2.3-6.7-1.1-1.5-.8-2.4-.8-3.9 0-2.5-1.2-4.4-3.4-4.4z"/>'
  };
  const T = {
    ja: { for: n => `${n}版をダウンロード`, wip: n => `${n}版は準備中`, other: 'ほかの環境', soon: '準備中', source: 'ソースから導入', get: 'ダウンロード', build: 'ビルド手順を見る', detected: 'お使いの環境', windowsSource: 'Windowsで使う' },
    en: { for: n => `Download for ${n}`, wip: n => `${n} coming soon`, other: 'Other platforms', soon: 'Coming soon', source: 'Install from source', get: 'Download', build: 'See build steps', detected: 'Your platform', windowsSource: 'Use on Windows' }
  };
  const lang = () => document.documentElement.lang === 'en' ? 'en' : 'ja';

  function detect() {
    const ua = navigator.userAgent || '';
    const p = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '';
    if (/android/i.test(ua)) return 'android';
    if (/iphone|ipad|ipod/i.test(ua) || (/mac/i.test(p) && navigator.maxTouchPoints > 1)) return 'ios';
    if (/win/i.test(p) || /windows/i.test(ua)) return 'win';
    if (/mac/i.test(p) || /mac os/i.test(ua)) return 'mac';
    if (/linux|x11/i.test(p + ua)) return 'linux';
    return 'mac';
  }
  const mine = detect();
  const svg = k => `<svg class="os" viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">${ICON[k]}</svg>`;
  const order = ['mac', 'ios', 'win', 'android', 'linux'];

  const isMob = mine === 'ios' || mine === 'android';
  document.documentElement.classList.toggle('mob', isMob);
  document.querySelectorAll('.dl-mob').forEach(box => {
    const order2 = mine === 'ios' ? ['ios', 'android'] : ['android', 'ios'];
    order2.forEach(k => box.insertBefore(box.querySelector(`[data-dl-mob="${k}"]`), box.querySelector('.also')));
  });

  function render() {
    const t = T[lang()];
    const d = DL[mine];
    document.querySelectorAll('[data-dl-mob]').forEach(a => {
      const k = a.dataset.dlMob, x = DL[k];
      a.href = x.url;
      a.classList.toggle('btn-ghost', !x.ready);
      a.innerHTML = svg(k === 'ios' ? 'mac' : k) + `<span>${x.cta ? x.cta[lang()] : x.ready ? t.for(x.name) : t.wip(x.name)}</span>`;
    });
    document.querySelectorAll('[data-dl-primary]').forEach(a => {
      a.href = d.url;
      a.innerHTML = svg(mine) + `<span>${d.cta ? d.cta[lang()] : mine === 'win' && d.source ? t.windowsSource : d.source ? t.source + ' · ' + d.name : d.ready ? t.for(d.name) : t.wip(d.name)}</span>`;
      a.classList.toggle('is-soon', !d.ready && !d.source);
    });
    document.querySelectorAll('[data-dl-toggle] span').forEach(s => {
      s.textContent = `${t.other}: ` + order.filter(k => k !== mine).map(k => DL[k].name.split(' ')[0]).join(' · ');
    });
    document.querySelectorAll('[data-dl-menu]').forEach(m => {
      m.innerHTML = order.filter(k => k !== mine).map(k => item(k, t)).join('');
    });
    document.querySelectorAll('[data-dl-grid]').forEach(g => {
      g.innerHTML = order.map(k => {
        const x = DL[k];
        const label = x.cta ? x.cta[lang()] : x.source ? t.source : x.ready ? t.get : t.build;
        return `<div class="dl-card${k === mine ? ' mine' : ''}${x.ready || x.source ? '' : ' soon'}">
          <div class="dl-ch">${svg(k)}${k === mine ? `<span class="tag">${t.detected}</span>` : ''}</div>
          <h3>${x.name}</h3><p>${x.file[lang()]}</p>
          <a class="btn ${x.ready ? '' : 'btn-ghost'} btn-sm" href="${x.url}">${label}</a>
          ${x.ready || x.source ? '' : `<span class="state">${t.soon}</span>`}
        </div>`;
      }).join('');
    });
  }
  function item(k, t) {
    const x = DL[k];
    return `<a class="dl-item" href="${x.url}">${svg(k)}<span><b>${x.name}</b><small>${x.file[lang()]}${x.ready || x.source ? '' : ' · ' + t.soon}</small></span></a>`;
  }

  document.querySelectorAll('[data-dl-toggle]').forEach(b => {
    const menu = b.parentNode.querySelector('[data-dl-menu]');
    b.addEventListener('click', e => { e.stopPropagation(); const open = menu.hidden; menu.hidden = !open; b.setAttribute('aria-expanded', String(open)); });
  });
  document.addEventListener('click', e => {
    document.querySelectorAll('[data-dl-menu]').forEach(m => { if (!m.contains(e.target)) { m.hidden = true; m.parentNode.querySelector('[data-dl-toggle]').setAttribute('aria-expanded', 'false'); } });
  });
  document.addEventListener('keydown', e => { if (e.key === 'Escape') document.querySelectorAll('[data-dl-menu]').forEach(m => m.hidden = true); });
  new MutationObserver(render).observe(document.documentElement, { attributes: true, attributeFilter: ['lang'] });
  render();
  // Publish a direct download only after GitHub reports the release asset.
  // A Mac-only release, an API failure or an unpublished ZIP keeps source setup available.
  async function loadReleaseDownloads() {
    try {
      const response = await fetch('https://api.github.com/repos/ttizze/remote-agent/releases/latest', {
        headers: { Accept: 'application/vnd.github+json' },
        signal: AbortSignal.timeout(5000)
      });
      if (!response.ok) return;
      const release = await response.json();
      for (const [platform, filename] of [['win', 'bex-windows-x86_64-release.zip']]) {
        const asset = release.assets?.find(x => x.name === filename && x.state === 'uploaded');
        if (!asset) continue;
        const url = new URL(asset.browser_download_url);
        if (url.origin !== 'https://github.com' || !url.pathname.startsWith('/ttizze/remote-agent/releases/download/')) continue;
        const target = DL[platform];
        target.url = url.href;
        target.ready = true;
        target.source = false;
        target.file = { ja: 'Windows 11 · x86-64 · ZIP', en: 'Windows 11 · x86-64 · ZIP' };
      }
      render();
    } catch { /* Keep the pinned, published preview download. */ }
  }
  loadReleaseDownloads();
})();
