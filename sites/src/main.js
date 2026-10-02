import '@fontsource/barlow-condensed/latin-600.css';
import '@fontsource/ibm-plex-mono/latin-400.css';
import './style.css';

// Copy: the clipboard can be refused (an insecure origin, a declined
// prompt); then the command is selected so the reader can copy it by hand.
for (const copy of document.querySelectorAll('.copy')) {
  copy.addEventListener('click', async () => {
    const text = document.getElementById(copy.dataset.copy);
    try {
      await navigator.clipboard.writeText(text.textContent);
      copy.textContent = 'Copied';
    } catch {
      const range = document.createRange();
      range.selectNodeContents(text);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      copy.textContent = 'Selected';
    }
    setTimeout(() => { copy.textContent = 'Copy'; }, 1600);
  });
}

// The install line for the reader's system: Windows gets the PowerShell
// installer, everything else install.sh. Both switches move together, and
// a Windows browser starts on Windows.
const commands = document.querySelectorAll('code[data-windows]');
function showOs(os) {
  for (const code of commands) {
    code.textContent = code.dataset[os];
    code.parentElement.querySelector('.install-prompt').textContent = os === 'windows' ? 'PS>' : '$';
  }
  for (const button of document.querySelectorAll('.os')) {
    button.setAttribute('aria-pressed', String(button.dataset.os === os));
  }
}
for (const button of document.querySelectorAll('.os')) {
  button.addEventListener('click', () => showOs(button.dataset.os));
}
const platform = navigator.userAgentData?.platform || navigator.platform || '';
if (/^win/i.test(platform)) showOs('windows');

// A download link goes to the newest release that carries the desktop app.
// GitHub is asked only when a link is clicked; when it does not answer, the
// link opens the releases page it already points at.
const RELEASES = 'https://api.github.com/repos/HarzerHeribert/sterna/releases?per_page=20';
for (const link of document.querySelectorAll('a[data-asset]')) {
  link.addEventListener('click', async (event) => {
    event.preventDefault();
    let url = link.href;
    try {
      const response = await fetch(RELEASES, { headers: { accept: 'application/vnd.github+json' } });
      const releases = response.ok ? await response.json() : [];
      const release = releases.find((r) => !r.draft && r.assets.some((a) => a.name.startsWith('sterna-desktop-')));
      const asset = release?.assets.find((a) => a.name.endsWith(link.dataset.asset));
      if (asset) url = asset.browser_download_url;
    } catch {
      // The releases page.
    }
    location.href = url;
  });
}

// The desktop showcase: the stage tilts in as it comes into view, its
// windows play the app at work and drift with the scroll at their own depth,
// the stage leans a little after the pointer, and a theme brings its window
// to the front. All of it rests while motion is paused or reduced; a window
// then shows its first frame.
const showcase = document.querySelector('.showcase');
if (showcase) {
  const fan = showcase.querySelector('.fan');
  const stage = showcase.querySelector('.stage');
  const layers = [...showcase.querySelectorAll('.layer')];
  const clips = [...showcase.querySelectorAll('video.win')];
  const themes = showcase.querySelectorAll('.theme');
  // How near each window stands: the nearer, the further it travels.
  const depth = { running: 0.25, overview: 0.6, session: 1, done: 1.45 };
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  const still = () => reduced.matches || document.querySelector('.motion-toggle')?.getAttribute('aria-pressed') === 'true';
  let seen = false;

  const front = (win) => {
    fan.dataset.front = win;
    for (const theme of themes) theme.setAttribute('aria-pressed', String(theme.dataset.win === win));
  };
  for (const theme of themes) {
    for (const event of ['click', 'mouseenter', 'focus']) theme.addEventListener(event, () => front(theme.dataset.win));
  }
  for (const layer of layers) layer.addEventListener('mouseenter', () => front(layer.dataset.win));

  const play = () => {
    for (const clip of clips) {
      if (seen && !still() && !document.hidden) {
        if (!clip.src) clip.src = clip.dataset.src;
        clip.play().catch(() => {});
      } else clip.pause();
    }
  };
  // One eased loop draws the scroll and the pointer: where the stage stands
  // in the window (-1 rising in from below, 1 leaving at the top) and how far
  // the pointer leans it, each followed a little behind, as a camera would.
  const place = () => {
    const box = stage.getBoundingClientRect();
    const middle = box.top + box.height / 2;
    return Math.max(-1, Math.min(1, (innerHeight / 2 - middle) / (innerHeight / 2 + box.height / 2)));
  };
  const shown = { open: 0, rx: 0, ry: 0 };
  const goal = { open: 0, rx: 0, ry: 0 };
  let drawing = false;
  const draw = () => {
    let moving = false;
    for (const key of Object.keys(shown)) {
      shown[key] += (goal[key] - shown[key]) * 0.08;
      if (Math.abs(goal[key] - shown[key]) < 0.0005) shown[key] = goal[key];
      else moving = true;
    }
    showcase.style.setProperty('--open', shown.open.toFixed(4));
    showcase.style.setProperty('--rx', `${shown.rx.toFixed(3)}deg`);
    showcase.style.setProperty('--ry', `${shown.ry.toFixed(3)}deg`);
    for (const layer of layers) {
      const near = depth[layer.dataset.win];
      layer.style.setProperty('--py', `${(-shown.open * near * 170).toFixed(1)}px`);
      layer.style.setProperty('--px', `${(shown.open * near * 46).toFixed(1)}px`);
    }
    drawing = moving;
    if (moving) requestAnimationFrame(draw);
  };
  const aim = () => {
    if (still()) Object.assign(goal, { open: 0, rx: 0, ry: 0 });
    else goal.open = place();
    if (!drawing) { drawing = true; requestAnimationFrame(draw); }
  };
  addEventListener('scroll', aim, { passive: true });
  addEventListener('resize', aim);

  showcase.classList.add('ready');
  new IntersectionObserver((entries) => {
    seen = entries.some((entry) => entry.isIntersecting);
    if (seen) showcase.classList.add('in');
    play();
  }, { threshold: 0.15 }).observe(showcase);
  document.addEventListener('visibilitychange', play);
  reduced.addEventListener('change', () => { play(); aim(); });
  // The page's Pause motion button sets its state when it is clicked.
  document.addEventListener('click', (event) => {
    if (event.target.closest?.('.motion-toggle')) setTimeout(() => { play(); aim(); });
  });

  stage.addEventListener('pointermove', (event) => {
    if (still()) return;
    const box = stage.getBoundingClientRect();
    goal.rx = -((event.clientY - box.top) / box.height - 0.5) * 4;
    goal.ry = ((event.clientX - box.left) / box.width - 0.5) * 6;
    aim();
  });
  stage.addEventListener('pointerleave', () => { goal.rx = 0; goal.ry = 0; aim(); });
  aim();
}

const toggle = document.querySelector('.motion-toggle');
import('./optics.js')
  .then(({ startOptics }) => startOptics([...document.querySelectorAll('.optics')], toggle))
  .catch(() => { toggle.hidden = true; });
