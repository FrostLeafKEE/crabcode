require('esbuild').buildSync({ entryPoints: ['extension.js'], bundle: true, outfile: 'dist/extension.js',
  external: ['vscode'], platform: 'node', format: 'cjs', target: 'ES2022', minify: true });
