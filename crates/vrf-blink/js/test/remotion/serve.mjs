import { createServer } from 'node:http';
import { createReadStream, statSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';

const TYPES = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.mp3': 'audio/mpeg',
  '.mp4': 'video/mp4',
  '.woff2': 'font/woff2',
  '.woff': 'font/woff',
  '.json': 'application/json',
  '.map': 'application/json',
};

export async function serve(root, { onRequest } = {}) {
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    if (onRequest?.(url, req, res)) return;
    const file = join(root, normalize(decodeURIComponent(url.pathname)));
    let st;
    try {
      st = statSync(file);
    } catch {
      res.writeHead(404).end();
      return;
    }
    if (!st.isFile()) {
      res.writeHead(404).end();
      return;
    }
    const headers = {
      'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
      'content-length': st.size,
      'accept-ranges': 'bytes',
    };
    const range = req.headers.range?.match(/bytes=(\d+)-(\d*)/);
    if (range) {
      const start = Number(range[1]);
      const end = range[2] ? Number(range[2]) : st.size - 1;
      res.writeHead(206, { ...headers, 'content-length': end - start + 1, 'content-range': `bytes ${start}-${end}/${st.size}` });
      createReadStream(file, { start, end }).pipe(res);
      return;
    }
    res.writeHead(200, headers);
    createReadStream(file).pipe(res);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    port: server.address().port,
    url: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}
