import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const [projectDir, bundleDir] = process.argv.slice(2).map((p) => resolve(p));
process.chdir(projectDir);
const { RenderInternals } = createRequire(`${projectDir}/package.json`)('@remotion/renderer');
const server = await RenderInternals.prepareServer({
  webpackConfigOrServeUrl: bundleDir,
  port: null,
  remotionRoot: projectDir,
  concurrency: 1,
  logLevel: 'error',
  indent: false,
  offthreadVideoCacheSizeInBytes: null,
  binariesDirectory: null,
  forceIPv4: true,
});
process.stdout.write(`${JSON.stringify({ serveUrl: server.serveUrl, offthreadPort: server.offthreadPort })}\n`);
process.stdin.resume();
process.stdin.on('end', async () => {
  await server.closeServer(true);
  process.exit(0);
});
