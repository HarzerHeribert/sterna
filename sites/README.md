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
| `index.html` | the whole page as static HTML; it reads the same with scripts off |
| `src/style.css` | the look |
| `src/main.js` | the fonts, the copy buttons, and loading the glass |
| `src/optics.js` | the glass over the specimens (three.js, timed with GSAP) |
| `public/specimens.jpg` | the three tern specimens, side by side (see below) |
| `public/mark.svg` | the favicon |
| `public/sterna-banner.png` | the social preview image |
| `public/install.sh` | the installer, served at `/install.sh`; it is part of the release tooling, not of the page |

Vite copies `public/` to the site root, so the installer's URL is
`https://harzerheribert.github.io/sterna/install.sh`. Links are relative
(`base: './'`), so the build works under the Pages prefix and on a custom
domain alike.

## The look

- The layout and type of the first Glasshouse site (2026-09-06): Barlow
  Condensed headlines with one word in outline, IBM Plex Mono for labels,
  Arial for reading. Both fonts are bundled from `@fontsource`; nothing is
  loaded from another server.
- The colours are the Arctic tern's, sampled from the photographs: its
  white for paper (`#f3f4f6`), its cap for ink (`#15191f`), its breast and
  mantle greys for rules and quiet text, and its bill's red (`#c8262b`) as
  the one accent. White on the red is about 5:1.
- The glass: a pane drifts slowly over each specimen, bends what is below
  it at a bevelled edge with a slight colour split and a lit rim, and
  follows the pointer a little. Every few seconds the drawing resolves into
  a red dot scan in steps and settles back. It draws only while it is on
  screen; "Pause motion" stops it, and `prefers-reduced-motion` starts it
  paused. Without WebGL the specimens show as still images.
- The install line is on the first screen at every size, and again under
  "Start in a minute". A long command scrolls inside its box; Copy takes
  the whole line.

## The specimens

`public/specimens.jpg` holds three 768 × 1024 panels, left to right: a
tern in flight, a tern with an eel, and a flock. Green holds the ink
density (white is ink); red is lowered where the bill and feet are, which
the shader draws in the bill's red. The panels are made from these
photographs, all CC BY-SA 4.0, credited in the footer:

| panel | photograph |
|---|---|
| in flight | [Arctic tern (Sterna paradisaea) in flight Myrar](https://commons.wikimedia.org/wiki/File:Arctic_tern_(Sterna_paradisaea)_in_flight_Myrar.jpg), Charles J. Sharp |
| with an eel | [Arctic tern (Sterna paradisaea) with eel Blonduos](https://commons.wikimedia.org/wiki/File:Arctic_tern_(Sterna_paradisaea)_with_eel_Blonduos.jpg), Charles J. Sharp |
| a flock | [Stormo di Sterna paradisaea](https://commons.wikimedia.org/wiki/File:Stormo_di_Sterna_paradisaea_-_buiobuione.jpg), Buiobuione |

The sky is separated by its blue (for the flock, by its brightness around
each bird), the bird's shade becomes ink density, and the bill and feet are
picked out by their red. The copy is plain: what Sterna does, what the
comparison with Codex means for someone using it (the method and every
number stay in `docs/measurements.md`, one link away), and how to start.
