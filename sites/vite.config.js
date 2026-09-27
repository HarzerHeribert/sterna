import { defineConfig } from 'vite';

// One static page. `base: './'` keeps every link relative, so the build works
// under the repository's Pages prefix (/sterna/) and on a custom domain alike.
export default defineConfig({
  base: './',
});
