// Builds dev/harness.tsx -> dev/dist (see harness.tsx).
const path = require('path');
const esbuild = require('esbuild');
const globalExternals = require('@fal-works/esbuild-plugin-global-externals');
const sassPlugin = require('esbuild-sass-plugin');
const postcss = require('postcss');
const postcssPrefixSelector = require('postcss-prefix-selector');

esbuild
  .build({
    entryPoints: [path.join(__dirname, 'harness.tsx')],
    bundle: true,
    outdir: path.join(__dirname, 'dist'),
    target: 'es2017',
    sourcemap: true,
    define: { BASE_URL: '"../src"' },
    plugins: [
      globalExternals.globalExternals({ '@microsoft/msfs-sdk': { varName: 'msfssdk', type: 'cjs' } }),
      sassPlugin.sassPlugin({
        async transform(source) {
          const { css } = await postcss([postcssPrefixSelector({ prefix: '.efb-view.BrowserApp' })]).process(source, { from: undefined });
          return css;
        },
      }),
    ],
  })
  .then(() => console.log('harness built: open dev/harness.html'))
  .catch(() => process.exit(1));
