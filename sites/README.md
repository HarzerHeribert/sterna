# Sterna product page

One static page, built with Vite and published by
`.github/workflows/pages.yml` on every push to `main` that touches
`sites/**`. The workflow runs `npm ci` and `npm run build` here and deploys
`sites/dist/`.

```sh
cd sites
npm ci
npm run dev       # http://127.0.0.1:5173
npm run build     # writes dist/
npm run preview   # serves dist/
```

## What is where

| path | what |
|---|---|
| `index.html` | the whole page, as static HTML: it reads the same with scripts off |
| `src/style.css` | the look |
| `src/main.js` | the fonts and the copy button, nothing else |
| `public/mark.svg` | the tern in flight: the favicon and the hero bird |
| `public/perched.svg` | the perched tern, in the themes section |
| `public/sterna-banner.png` | the social preview image (the README banner) |
| `public/install.sh` | the installer, served at `/install.sh`; it is part of the release tooling, not of the page |

Vite copies `public/` to the site root, so the installer's URL is
`https://harzerheribert.github.io/sterna/install.sh`. Links are relative
(`base: './'`), so the build works under the Pages prefix and on a custom
domain alike.

## The look

- One family: JetBrains Mono, bundled from `@fontsource/jetbrains-mono`; no
  font or script is loaded from another server.
- Terminal black `#0A0E12` with the banner's dot grid, ice `#E9EEF2` for
  headings and code, fog `#9AA6B0` for text, dim `#7F8C98` for notes, and
  the beak red `#E0473B` as the only colour. Every foreground clears 4.5:1
  on the background; nothing is drawn black on black.
- The tern sprites are the ones in `docs/assets/`, traced from photographs;
  the credits are in the repository README.
- Works at phone width: 16 px gutters, no horizontal page scroll. The
  cursor stops blinking under `prefers-reduced-motion`.
- The content is the README's. When a claim changes there, change it here.
