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

const toggle = document.querySelector('.motion-toggle');
import('./optics.js')
  .then(({ startOptics }) => startOptics([...document.querySelectorAll('.optics')], toggle))
  .catch(() => { toggle.hidden = true; });
