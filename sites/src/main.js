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
  const depth = { running: 0.3, overview: 0.55, session: 0.85, done: 1.15 };
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
  const drift = () => {
    const box = stage.getBoundingClientRect();
    const away = Math.max(-1, Math.min(1, (box.top + box.height / 2 - innerHeight / 2) / innerHeight));
    for (const layer of layers) {
      layer.style.setProperty('--py', still() ? '0px' : `${(away * depth[layer.dataset.win] * 80).toFixed(1)}px`);
    }
  };
  let drawing = false;
  addEventListener('scroll', () => {
    if (drawing) return;
    drawing = true;
    requestAnimationFrame(() => { drawing = false; drift(); });
  }, { passive: true });

  showcase.classList.add('ready');
  new IntersectionObserver((entries) => {
    seen = entries.some((entry) => entry.isIntersecting);
    if (seen) showcase.classList.add('in');
    play();
  }, { threshold: 0.15 }).observe(showcase);
  document.addEventListener('visibilitychange', play);
  reduced.addEventListener('change', () => { play(); drift(); });
  // The page's Pause motion button sets its state when it is clicked.
  document.addEventListener('click', (event) => {
    if (event.target.closest?.('.motion-toggle')) setTimeout(() => { play(); drift(); });
  });

  const lean = (rx, ry) => {
    showcase.style.setProperty('--rx', `${rx.toFixed(2)}deg`);
    showcase.style.setProperty('--ry', `${ry.toFixed(2)}deg`);
  };
  stage.addEventListener('pointermove', (event) => {
    if (still()) return;
    const box = stage.getBoundingClientRect();
    lean(-((event.clientY - box.top) / box.height - 0.5) * 3, ((event.clientX - box.left) / box.width - 0.5) * 4);
  });
  stage.addEventListener('pointerleave', () => lean(0, 0));
  drift();
}

const toggle = document.querySelector('.motion-toggle');
import('./optics.js')
  .then(({ startOptics }) => startOptics([...document.querySelectorAll('.optics')], toggle))
  .catch(() => { toggle.hidden = true; });
