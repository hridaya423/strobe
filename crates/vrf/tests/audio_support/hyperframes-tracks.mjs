import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [hyperframesDir, indexHtml] = process.argv.slice(2);
const dist = join(hyperframesDir, 'dist');
const chunk = readdirSync(dist).find((name) => name.endsWith('.js') && readFileSync(join(dist, name), 'utf8').includes('function parseAudioElements('));
const { parseAudioElements } = await import(pathToFileURL(join(dist, chunk)).href);
process.stdout.write(JSON.stringify(parseAudioElements(readFileSync(indexHtml, 'utf8'))));
