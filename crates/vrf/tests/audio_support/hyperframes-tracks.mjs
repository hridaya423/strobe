import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [hyperframesDir, projectDir] = process.argv.slice(2);
const dist = join(hyperframesDir, 'dist');
const chunk = readdirSync(dist).find((name) => name.endsWith('.js') && readFileSync(join(dist, name), 'utf8').includes('function parseAudioElements('));
const { parseAudioElements } = await import(pathToFileURL(join(dist, chunk)).href);

const attribute = (tag, name) => tag.match(new RegExp(`\\s${name}="([^"]*)"`))?.[1];

const collect = (file, offset, limit) => {
  const html = readFileSync(join(projectDir, file), 'utf8');
  const tracks = parseAudioElements(html).map((track) => {
    const start = track.start + offset;
    const end = track.end > 0 ? Math.min(track.end + offset, limit) : 0;
    return { ...track, start, end };
  });
  for (const [tag] of html.matchAll(/<[^>]*data-composition-src="[^"]+"[^>]*>/g)) {
    const src = attribute(tag, 'data-composition-src');
    const start = Number(attribute(tag, 'data-start') ?? 0) + offset;
    const duration = Number(attribute(tag, 'data-duration') ?? Infinity);
    tracks.push(...collect(src, start, Math.min(limit, start + duration)));
  }
  return tracks;
};

process.stdout.write(JSON.stringify(collect('index.html', 0, Infinity)));
