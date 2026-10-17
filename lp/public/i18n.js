(() => {
  const en = {
    'strength.native.title': 'A responsive native app.',
    'strength.native.body': 'The desktop app uses Rust and GPUI. Unchanged presentation data is reused to avoid unnecessary work.',
    'strength.connection.title': 'Disconnect. Then carry on.',
    'strength.connection.body': 'Work keeps running on your computer when your device disconnects. Reconnect to pick up from the latest state.',
    'visual.note': 'UI illustrations show the features and setup flow.',
    'simple.local.body': 'Code stays with you. Connections are encrypted.',
    'simple.local.title': 'Your computer does the work.',
    'simple.mobile.body': 'The same conversations, instructions and approvals.',
    'simple.mobile.title': 'Pick up from your phone.',
    'simple.agents.body': 'Your existing accounts. One app.',
    'simple.agents.title': 'Codex and Claude Code.',
    'p.android.version': 'Android 8.0+',
    'demo.eyebrow': 'SEE BEX IN ACTION', 'demo.title': 'A quick look at how it works.',
    'demo.sub': 'Choose an agent, review changes, and approve commands from your phone.',
    'demo.tab0': 'Choose an agent', 'demo.tab1': 'Review changes', 'demo.tab2': 'Approve on mobile',
    'demo.note': 'Recreated demo · Sample data',
    'demo.fallback': 'Open the video',

    'nav.screens': 'Screens', 'nav.agents': 'Two agents',
    'ag.eyebrow': 'Two agents, one app', 'ag.title': 'Codex and Claude Code,<br> in one app.',
    'ag.sub': 'No more juggling apps. Tap the model name in the composer to pick the agent and the account.',
    'ag1.t': 'Keep your subscriptions', 'ag1.d': 'Use your ChatGPT and Claude accounts as they are. Each agent manages its own sign-in.',
    'ag2.t': 'Switch between accounts', 'ag2.d': 'See each account\'s weekly usage at a glance. Your history stays when you switch.',
    'ag3.t': 'One way of working', 'ag3.d': 'Attachments, approvals, answering questions, interrupting and worktrees all work the same with either agent.',
    'ag.pick': 'Choose a model', 'ag.search': 'Search', 'ag.acc1': 'Work ChatGPT', 'ag.week': 'This week', 'ag.acc2': 'Team API key', 'ag.api': 'Billed by API usage',
    'ag.acc3': 'Personal Claude', 'ag.add': 'Add account', 'ag.effort': 'Reasoning: High',
    'dl.also': 'Also on macOS, Windows and Linux', 'dl.eyebrow': 'Download', 'dl.title': 'Download bex',
    'dl.sub': 'Install the Host and desktop app on your computer and the app on your phone, then pair them with a QR code.',
    'dl.note': 'On Windows, extract the ZIP and run bex-desktop.exe. Git, Node.js and your chosen agent are required.',
    'sc.eyebrow': 'Screens', 'sc.title': 'Mac or phone,<br class="sp"> the same conversation.',
    'sc.sub': 'At your desk, use the Mac app with diffs and a terminal side by side. Away from it, carry on with the same task on iPhone or Android.',
    'sc.note': 'Screens are recreations based on the app\'s interface.',
    'm.mdl': 'Codex · High', 'm.new': 'New conversation', 'm.projects': 'Projects', 'm.chats': 'Chats',
    'm.t1': 'Fix the checkout tests', 'm.t2': 'Paginate the inventory API', 'm.t3': 'Translate header copy', 'm.t4': 'Retry on login', 'm.t5': 'Draft release notes',
    'm.running': 'Running 2:14 · running tests',
    'm.u1': 'The checkout E2E tests are failing. Fix them and tell me why.',
    'm.a1': 'The total was rounded after the coupon but before tax. I fixed the order of the calculation and added boundary tests.',
    'm.pass': 'Passed · 24', 'm.files': '3 files', 'm.diff': 'View changes', 'm.ask': 'Reply…', 'm.term': 'Terminal',
    'p.conn': 'Connected', 'p.tasks': 'Tasks', 'p.r1': 'Running tests', 'p.r2': 'Waiting for command approval', 'p.r3': 'Done · unread', 'p.r4': 'Merged',
    'p.u': 'Return the inventory list 50 items at a time', 'p.a': 'Implemented with cursors. Before running the tests, please confirm the migration.',
    'p.rq': 'Waiting for command approval', 'p.c_1': '1. Allow', 'p.c_2': '2. Always allow for this task', 'p.c_3': '3. Deny and give instructions',
    'p.date': 'Tuesday, October 14', 'p.la': 'Running tests · 2:14', 'p.la2': '1 more waiting for approval',
    'p.c1': 'Task list', 'p.c2': 'Answer approvals and questions', 'p.c3': 'Lock Screen and Dynamic Island',
    'nav.features': 'Features', 'nav.how': 'How it works', 'nav.security': 'Security', 'nav.platforms': 'Platforms',
    'cta.download.short': 'Download', 'cta.download': 'Download prd-74', 'cta.github': 'View on GitHub', 'cta.docs': 'Docs',
    'hero.pill': 'Production build 74 is out',
    'hero.title': 'Your coding agents,<br class="sp"> anywhere.',
    'hero.lead': 'Codex and Claude Code in one app. Keep the same work moving, on your computer or your phone.',
    'hero.works': 'Works with Codex and Claude Code · macOS · iOS · Android · Windows · Linux',
    'hero.alt': 'A desk computer running a coding agent, connected to a phone in the user\'s hand',
    'strip.1': 'Host, every device connected to it', 'strip.2': 'agents: Codex and Claude Code', 'strip.3': 'platforms: Mac, iPhone, Android, Windows, Linux',
    'f.eyebrow': 'Features', 'f.title': 'Keep building. From anywhere.',
    'f.sub': 'The work always happens on your computer. bex lets you pick it up from whatever screen is in front of you.',
    'f1.t': 'Continue from your pocket.',
    'f1.d': 'Start a conversation on your Mac and carry on from iPhone or Android. Task status shows on the Lock Screen and in the Dynamic Island; tap it to jump straight back in.',
    'f1.l1': 'Same conversations and drafts on every device', 'f1.l2': 'Live Activities show progress on iPhone', 'f1.l3': 'Runs keep going when you disconnect; reconnect to the latest state',
    'f1.alt': 'A phone showing an agent conversation, away from the desk',
    'f2.t': 'Ask about any passage', 'f2.d': 'Select part of an answer to quote it in your draft or ask about it in a side chat. The main conversation stays put.',
    'f3.t': 'A worktree for every task', 'f3.d': 'Each new task runs in its own Git worktree so work never collides. Merged ones can clean themselves up.',
    'f4.t': 'Just say it', 'f4.d': 'Dictation drops what you said into the draft. If recording fails, what you already typed stays put.',
    'f5.t': 'Approve and answer on the go', 'f5.d': 'Grant command approvals and answer your agent\'s questions from your phone, so it is never left waiting.',
    'f6.t': 'Files, diffs and a terminal', 'f6.d': 'Open diffs from change summaries, edit files on the Host and drive a native terminal.',
    'f7.t': 'A browser that stays signed in', 'f7.d': 'Import Chrome cookies so bex\'s browser can check the same sites while you stay logged in.',
    'h.eyebrow': 'How it works', 'h.title': 'Connect. Then carry on.',
    'h1.t': 'Open bex', 'h1.d': 'Open bex on the computer you work on.',
    'h2.t': 'Pair with a QR code', 'h2.d': 'Choose Add PC on your phone and scan the QR code.',
    'h3.t': 'Steer from anywhere', 'h3.d': 'Open the same conversation and pick up where you left off.',
    's.eyebrow': 'Security', 's.title': 'Your code stays<br class="sp"> on your computer.',
    's.sub': 'bex never parks your code in someone else\'s cloud. Agents run only on a computer you trust.',
    's1.t': 'Encrypted end to end', 's1.d': 'Devices connect to the Host over iroh QUIC. When traffic goes through a relay, the relay cannot decrypt your conversations.',
    's2.t': 'Only devices you pair', 's2.d': 'One invitation, one device. Revoke any device at any time. Identity keys live in an owner-only file.',
    's3.t': 'Each agent keeps its own sign-in', 's3.d': 'Codex and Claude Code manage their own credentials. bex never copies tokens or calls another API behind your back.',
    's4.t': 'No ads, no tracking', 's4.d': 'bex has no advertising and no advertising tracking.',
    'p.eyebrow': 'Platforms', 'p.title': 'On the screen you already use.',
    'p.mac': 'Desktop app, Host and CLI', 'p.ios': 'Native app with Live Activities', 'p.android': 'Native app', 'p.win': 'Desktop, Host and CLI', 'p.linux': 'Headless Host and CLI',
    'c.title': 'Take your agents with you.', 'c.sub': 'Production build 74 (October 9, 2026) is available now.',
    'ft.releases': 'Releases', 'ft.privacy': 'Privacy',
    'ft.note': 'Codex is a product of OpenAI and Claude Code is a product of Anthropic. bex is not affiliated with either company.'
  };
  const meta = {
    ja: { title: 'bex — どこでも、これひとつで。' },
    en: { title: 'bex — Your coding agents, anywhere.' }
  };
  const ja = {};
  document.querySelectorAll('[data-i]').forEach(el => { ja[el.dataset.i] = el.innerHTML; });
  document.querySelectorAll('[data-i-alt]').forEach(el => { ja[el.dataset.iAlt] = el.alt; });
  ja['hero.alt'] = 'デスクのコンピューターで動くエージェントと、手元のスマホがつながっているイラスト';
  ja['f1.alt'] = '外出先でエージェントとの会話を開いているスマホのイラスト';

  function apply(lang) {
    const dict = lang === 'en' ? en : ja;
    document.documentElement.lang = lang;
    document.title = meta[lang].title;
    document.querySelectorAll('[data-i]').forEach(el => { const v = dict[el.dataset.i]; if (v != null) el.innerHTML = v; });
    document.querySelectorAll('[data-i-alt]').forEach(el => { const v = dict[el.dataset.iAlt]; if (v != null) el.alt = v; });
    document.querySelectorAll('[data-lang]').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.lang === lang)));
    try { localStorage.setItem('bex-lang', lang); } catch (e) {}
  }

  let initial = null;
  const q = new URLSearchParams(location.search).get('lang');
  if (q === 'ja' || q === 'en') initial = q;
  if (!initial) { try { initial = localStorage.getItem('bex-lang'); } catch (e) {} }
  if (initial !== 'ja' && initial !== 'en') initial = (navigator.language || '').toLowerCase().startsWith('ja') ? 'ja' : 'en';
  apply(initial);
  document.querySelectorAll('[data-lang]').forEach(b => b.addEventListener('click', () => apply(b.dataset.lang)));

  const nav = document.querySelector('.nav');
  const onScroll = () => nav.classList.toggle('scrolled', scrollY > 8);
  addEventListener('scroll', onScroll, { passive: true }); onScroll();

})();
