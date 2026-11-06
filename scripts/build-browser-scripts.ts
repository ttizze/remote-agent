import { transform } from 'esbuild';
import { glob, readFile, writeFile } from 'node:fs/promises';

// These scripts are evaluated as expressions by Rust/WebKit, or loaded by a
// browser/Maestro. Type erasure must preserve their final expression's value.
const { include } = JSON.parse(await readFile('tsconfig.browser.json', 'utf8')) as { include: string[] };
for await (const path of glob(include)) {
  if (path.endsWith('.d.ts')) continue;
  const sourcefile = path.replaceAll('\\', '/');
  const { code } = await transform(await readFile(sourcefile, 'utf8'), {
    sourcefile, loader: 'ts', target: 'es2022', treeShaking: false,
  });
  await writeFile(sourcefile.replace(/\.ts$/, '.generated.js'),
    `// Generated from ${sourcefile}; run npm run build.\n${code.trimEnd().replace(/;$/, '')}\n`);
}
