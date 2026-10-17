(() => {
  const section = document.querySelector('#demo');
  if (!section) return;
  const tabs = [...section.querySelectorAll('[role="tab"]')];
  const panels = tabs.map(tab => document.getElementById(tab.getAttribute('aria-controls')));
  const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
  let active = 0;
  let visible = false;
  function videoAt(index) { return panels[index].querySelector('video'); }
  function syncPlayback() {
    panels.forEach((_, index) => {
      const video = videoAt(index);
      if (index !== active || !visible || document.hidden || reducedMotion.matches) video.pause();
      else video.play().catch(() => {});
    });
  }
  function select(index) {
    active = index;
    tabs.forEach((tab, i) => {
      tab.setAttribute('aria-selected', String(i === index));
      tab.tabIndex = i === index ? 0 : -1;
      panels[i].hidden = i !== index;
    });
    const video = videoAt(index);
    if (video.dataset.src) { video.src = video.dataset.src; delete video.dataset.src; video.load(); }
    syncPlayback();
  }
  tabs.forEach((tab, index) => {
    tab.addEventListener('click', () => select(index));
    tab.addEventListener('keydown', event => {
      let next;
      if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
      if (event.key === 'ArrowLeft') next = (index + tabs.length - 1) % tabs.length;
      if (event.key === 'Home') next = 0;
      if (event.key === 'End') next = tabs.length - 1;
      if (next === undefined) return;
      event.preventDefault(); select(next); tabs[next].focus();
    });
  });
  if ('IntersectionObserver' in window) {
    new IntersectionObserver(entries => {
      visible = entries[0].isIntersecting; syncPlayback();
    }, { threshold: 0.25 }).observe(section.querySelector('.demo-stage'));
  }
  document.addEventListener('visibilitychange', syncPlayback);
  reducedMotion.addEventListener('change', syncPlayback);
})();
