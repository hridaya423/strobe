import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const [projectDir, entry, outDir] = process.argv.slice(2).map((p) => resolve(p));
process.chdir(projectDir);
const { bundle } = createRequire(`${projectDir}/package.json`)('@remotion/bundler');
const out = await bundle({ entryPoint: entry, outDir, enableCaching: false });
process.stdout.write(`${out}\n`);
