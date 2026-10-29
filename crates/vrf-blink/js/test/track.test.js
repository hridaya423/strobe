import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { after, before, describe, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { launch, serve } from './harness.js';

const here = dirname(fileURLToPath(import.meta.url));
const source = (name) => readFileSync(join(here, '..', name), 'utf8');
const scripts = [source('tick.js'), source('track.js')];

const dir = mkdtempSync(join(tmpdir(), 'vrf-track-'));
const gif = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';
writeFileSync(
  join(dir, 'index.html'),
  `<!doctype html><html><head><style id="sheet">#box{color:red} @keyframes fade{to{opacity:.5}}</style></head>
<body style="margin:0"><div id="box" style="width:50px;height:50px">text</div><div id="host"></div>
<div id="scroller" style="width:50px;height:50px;overflow:auto"><div style="height:500px"></div></div></body></html>`,
);

let browser;
let site;
before(async () => {
  site = await serve(dir, { '/': join(dir, 'index.html') });
  browser = await launch();
});
after(async () => {
  await browser.close();
  site.close();
});

async function open({ clock = false } = {}) {
  const page = await browser.newPage({ clock });
  for (const source of scripts) await page.send('Page.addScriptToEvaluateOnNewDocument', { source });
  await page.goto(`${site.origin}/index.html`);
  await page.evaluate('document.fonts.ready.then(() => 0)');
  const take = () => page.evaluate('__vrf.track.take()');
  await take();
  assert.equal(await take(), null);
  return { page, take };
}

const unchanged = (name, script) =>
  test(`track_unchanged_${name}`, async () => {
    const { page, take } = await open();
    await page.evaluate(script);
    assert.equal(await take(), null);
    await page.close();
  });

const changed = (name, script, reason) =>
  test(`track_changed_${name}`, async () => {
    const { page, take } = await open();
    await page.evaluate(script);
    assert.match(String(await take()), reason);
    assert.equal(await take(), null, 'the baseline resets after a take');
    await page.close();
  });

describe('quiet pages', () => {
  unchanged('nothing_happens', '0');
  unchanged('attribute_set_to_the_same_value', 'document.getElementById("box").setAttribute("id", "box"); 0');
  unchanged('inline_style_set_to_the_same_value', 'document.getElementById("box").style.width = "50px"');
  unchanged('attribute_changed_and_restored', 'const b = document.getElementById("box"); b.setAttribute("data-x", "1"); b.removeAttribute("data-x")');
  unchanged('text_changed_and_restored', 'const t = document.getElementById("box").firstChild; t.data = "other"; t.data = "text"');
  unchanged('the_tick_element_alternating', 'new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))');
});

describe('changes', () => {
  changed('inline_style', 'document.getElementById("box").style.opacity = "0.5"', /style/);
  changed('inline_style_by_attribute', 'document.getElementById("box").setAttribute("style", "width:51px")', /style/);
  changed('class', 'document.getElementById("box").className = "x"', /class/);
  changed('text', 'document.getElementById("box").firstChild.data = "other"', /#data/);
  changed('child_added', 'document.getElementById("box").appendChild(document.createElement("i"))', /structure/);
  changed('child_removed', 'document.getElementById("box").firstChild.remove()', /structure/);
  changed('document_element_class', 'document.documentElement.className = "dark"', /class/);
  changed('stylesheet_rule_inserted', 'document.getElementById("sheet").sheet.insertRule("#box{color:blue}")', /unobserved/);
  changed('stylesheet_rule_edited', 'document.getElementById("sheet").sheet.cssRules[0].style.color = "blue"', /unobserved/);
  changed('stylesheet_rule_property_set', 'document.getElementById("sheet").sheet.cssRules[0].style.setProperty("color", "blue")', /unobserved/);
  changed('stylesheet_text', 'document.getElementById("sheet").textContent = "#box{color:blue}"', /structure|#data/);
  changed('adopted_stylesheet', 'document.adoptedStyleSheets = [new CSSStyleSheet()]', /unobserved/);
  changed('scroll_top', 'document.getElementById("scroller").scrollTop = 20', /unobserved/);
  changed('window_scroll', 'window.scrollTo(0, 0)', /unobserved/);
  changed('animate_call', 'document.getElementById("box").animate([{opacity: 1}, {opacity: 0.2}], {duration: 1, fill: "forwards"}).finish(); 0', /unobserved|animation/);
});

describe('shadow roots', () => {
  test('track_closed_shadow_root_content_is_observed', async () => {
    const { page, take } = await open();
    await page.evaluate('window.__root = document.getElementById("host").attachShadow({mode: "closed"}); 0');
    await take();
    assert.equal(await take(), null);
    await page.evaluate('window.__root.innerHTML = "<b>x</b>"');
    assert.equal(await take(), 'structure');
    await page.close();
  });
});

describe('running state', () => {
  test('track_running_animation_blocks_after_baseline', async () => {
    const { page, take } = await open();
    await page.evaluate('document.getElementById("box").style.animation = "fade 100s infinite"');
    await take();
    assert.equal(await take(), 'animation');
    await page.close();
  });
  test('track_running_transition_blocks', async () => {
    const { page, take } = await open();
    await page.evaluate('document.getElementById("box").style.transition = "opacity 100s"');
    await take();
    await page.evaluate('document.getElementById("box").style.opacity = "0.1"');
    await take();
    assert.equal(await take(), 'animation');
    await page.close();
  });
  test('track_canvas_blocks', async () => {
    const { page, take } = await open();
    await page.evaluate('document.body.appendChild(document.createElement("canvas")); 0');
    await take();
    assert.equal(await take(), 'element');
    await page.evaluate('document.querySelector("canvas").remove()');
    await take();
    assert.equal(await take(), null);
    await page.close();
  });
  for (const tag of ['video', 'iframe', 'embed', 'object']) {
    test(`track_${tag}_blocks`, async () => {
      const { page, take } = await open();
      await page.evaluate(`document.body.appendChild(document.createElement("${tag}")); 0`);
      await take();
      assert.equal(await take(), 'element');
      await page.close();
    });
  }
  test('track_svg_smil_blocks', async () => {
    const { page, take } = await open();
    await page.evaluate(
      'document.body.insertAdjacentHTML("beforeend", "<svg width=10 height=10><rect width=5 height=5><animate attributeName=x from=0 to=5 dur=1s repeatCount=indefinite /></rect></svg>")',
    );
    await take();
    assert.equal(await take(), 'element');
    await page.close();
  });
  test('track_animated_image_formats_block', async () => {
    const { page, take } = await open();
    await page.evaluate(`new Promise((resolve) => { const i = new Image(); i.onload = resolve; i.src = ${JSON.stringify(gif)}; document.body.appendChild(i); })`);
    await take();
    assert.equal(await take(), 'image');
    await page.close();
  });
  test('track_stylesheet_with_animated_background_blocks', async () => {
    const { page, take } = await open();
    await page.evaluate(`document.getElementById("sheet").sheet.insertRule("#box{background:url(${gif})}")`);
    await take();
    assert.equal(await take(), 'stylesheet');
    await page.close();
  });
  test('track_inline_animated_background_blocks', async () => {
    const { page, take } = await open();
    await page.evaluate(`document.getElementById("box").style.backgroundImage = "url(a.gif)"`);
    await take();
    assert.equal(await take(), 'image');
    await page.close();
  });
  test('track_static_image_does_not_block_once_loaded', async () => {
    const { page, take } = await open();
    const png = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';
    await page.evaluate(`new Promise((resolve) => { const i = new Image(); i.onload = resolve; i.src = ${JSON.stringify(png)}; document.body.appendChild(i); })`);
    await take();
    assert.equal(await take(), null);
    await page.close();
  });
  test('track_unloaded_image_blocks_and_its_load_is_a_change', async () => {
    const { page, take } = await open();
    const png = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';
    const first = await page.evaluate(
      `(() => { const i = new Image(); i.src = ${JSON.stringify(png)}; document.body.appendChild(i); window.__img = i; return __vrf.track.take(); })()`,
    );
    assert.ok(first);
    await page.evaluate('new Promise((r) => (__img.complete ? r() : __img.addEventListener("load", r, { once: true })))');
    await page.evaluate('new Promise((r) => setTimeout(r, 20))');
    assert.equal(await take(), 'unobserved');
    assert.equal(await take(), null);
    await page.close();
  });
});

describe('the virtual clock', () => {
  test('track_runs_beside_the_clock_without_noise', async () => {
    const { page, take } = await open({ clock: true });
    for (let frame = 0; frame < 5; frame++) {
      await page.evaluate(`__vrf.clock.set(${frame}, 30, 1)`);
      assert.equal(await take(), null, `frame ${frame}`);
    }
    await page.close();
  });
});
