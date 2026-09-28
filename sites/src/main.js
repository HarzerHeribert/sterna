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

const toggle = document.querySelector('.motion-toggle');
import('./optics.js')
  .then(({ startOptics }) => startOptics([...document.querySelectorAll('.optics')], toggle))
  .catch(() => { toggle.hidden = true; });
