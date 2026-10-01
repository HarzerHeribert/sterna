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

const toggle = document.querySelector('.motion-toggle');
import('./optics.js')
  .then(({ startOptics }) => startOptics([...document.querySelectorAll('.optics')], toggle))
  .catch(() => { toggle.hidden = true; });
