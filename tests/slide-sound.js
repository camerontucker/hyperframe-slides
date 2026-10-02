'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const audios = [];
const listeners = new Map();
const changes = new Set();
const scenes = new Map([
  ['quiet-scene', { querySelector: () => null }],
  ['modem-scene', { querySelector: () => ({ dataset: { slideSoundSrc: 'data:audio/ogg;base64,test' } }) }],
]);
class Slideshow {
  muted = false;
  bindController(controller) { this.controller = controller; }
  querySelector() { return { iframe: { contentDocument: { getElementById: id => scenes.get(id) } } }; }
  addEventListener(name, callback) { listeners.set(name, callback); }
  removeEventListener(name, callback) { if (listeners.get(name) === callback) listeners.delete(name); }
}
class Audio {
  dataset = {};
  paused = true;
  removed = false;
  plays = 0;
  constructor(src) { this.src = src; audios.push(this); }
  play() { this.paused = false; this.plays++; return Promise.resolve(); }
  pause() { this.paused = true; }
  remove() { this.removed = true; }
}
vm.runInNewContext(fs.readFileSync('assets/slide-sound.js', 'utf8'), {
  customElements: { get: () => Slideshow }, Audio,
  document: { body: { appendChild() {} } },
  window: { addEventListener() {}, removeEventListener() {} },
});
const controller = { currentSlide: { sceneId: 'quiet' }, onChange(fn) { changes.add(fn); return () => changes.delete(fn); } };
const show = new Slideshow();
const change = sceneId => { controller.currentSlide = { sceneId }; for (const fn of changes) fn(); };
show.bindController(controller);
assert.equal(audios.length, 0);
change('modem');
assert.equal(audios.length, 1);
assert.equal(audios[0].paused, false);
change('modem'); // Fragment changes stay on the current slide.
assert.equal(audios[0].plays, 1);
show.muted = true; listeners.get('hf-sound')();
assert.equal(audios[0].muted, true);
change('quiet');
assert.equal(audios[0].paused, true);
assert.equal(audios[0].removed, true);
change('modem');
assert.equal(audios.length, 2);
assert.equal(audios[1].muted, true);
show.muted = false; listeners.get('hf-sound')();
assert.equal(audios[1].muted, false);
show.bindController(controller); // A rebound controller stops the old audio and leaves one listener.
assert.equal(audios[1].paused, true);
assert.equal(changes.size, 1);
show.slideSoundCleanup();
assert.equal(changes.size, 0);
assert.equal(audios.at(-1).paused, true);
assert.equal(listeners.has('hf-sound'), false);
console.log('Slide sound: entry, fragments, exit, revisit, mute and cleanup passed.');
