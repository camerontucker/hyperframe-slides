// Slide audio uses the presentation's clock, independently of the layout timeline.
(() => {
  const Slideshow = customElements.get('hyperframes-slideshow');
  if (!Slideshow || !Slideshow.prototype.bindController) return;
  const bindController = Slideshow.prototype.bindController;
  Slideshow.prototype.bindController = function (controller) {
    this.slideSoundCleanup?.();
    const result = bindController.call(this, controller);
    const player = this.querySelector('hyperframes-player');
    const frame = player?.iframe
      || player?.querySelector('iframe')
      || player?.shadowRoot?.querySelector('iframe');
    let sceneId = null;
    let sound = null;
    const stop = () => {
      if (sound) {
        sound.pause();
        sound.remove();
        sound = null;
      }
    };
    const play = audio => {
      audio.play().catch(error => {
        // Ignore a play request cancelled by immediately leaving the slide.
        if (audio === sound) audio.dataset.playbackError = error.name || 'PlaybackError';
      });
    };
    const sync = () => {
      const next = controller.currentSlide?.sceneId;
      if (next === sceneId) return; // Revealing a fragment does not replay the sound.
      sceneId = next;
      stop();
      const scene = frame?.contentDocument?.getElementById(`${next}-scene`);
      const source = scene?.querySelector('[data-slide-sound-src]')?.dataset.slideSoundSrc;
      if (!source) return;
      sound = new Audio(source);
      sound.dataset.slideSound = next;
      sound.volume = .6;
      sound.muted = this.muted;
      sound.hidden = true;
      document.body.appendChild(sound);
      play(sound);
    };
    const mute = () => {
      if (!sound) return;
      sound.muted = this.muted;
      if (!sound.muted && sound.dataset.playbackError === 'NotAllowedError') {
        delete sound.dataset.playbackError;
        play(sound);
      }
    };
    const offChange = controller.onChange(sync);
    this.addEventListener('hf-sound', mute);
    window.addEventListener('pagehide', stop);
    this.slideSoundCleanup = () => {
      offChange();
      this.removeEventListener('hf-sound', mute);
      window.removeEventListener('pagehide', stop);
      stop();
    };
    sync();
    return result;
  };
})();
