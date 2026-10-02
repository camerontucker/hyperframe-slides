// Keep HyperFrames on the slide's finished frame. Animating the whole deck's
// timeline on every display frame makes text entrances stutter on large decks.
// Drive only the heading/body and pictures. HyperFrames owns and pauses WAAPI
// animations inside its composition, so use the audience window's clock here.
(() => {
  const Slideshow = customElements.get('hyperframes-slideshow');
  if (!Slideshow || !Slideshow.prototype.bindController) return;

  const bindController = Slideshow.prototype.bindController;
  Slideshow.prototype.bindController = function (controller) {
    const seek = controller.playTo.bind(controller);
    let shownTime = controller.currentSlide?.fragments?.[0]
      ?? (controller.currentSlide ? controller.restFrame(controller.currentSlide) : null);
    let shownScene = controller.currentSlide?.sceneId;
    let firstAudienceSync = this.resolveMode() === 'audience';
    let entranceFrame = null;
    let targets = [];
    let entranceGeneration = 0;

    const stopEntrance = () => {
      entranceGeneration++;
      if (entranceFrame !== null) cancelAnimationFrame(entranceFrame);
      entranceFrame = null;
      for (const target of targets) {
        target.element.style.opacity = '1';
        if (target.distance || target.zoom) target.element.style.transform = 'none';
      }
      targets = [];
      delete this.dataset.hfTextEntranceActive;
    };

    const easeOut = progress => 1 - (1 - progress) ** 3;
    const clamp = value => Math.max(0, Math.min(1, value));

    controller.playTo = (time) => {
      if (firstAudienceSync) {
        firstAudienceSync = false;
        stopEntrance();
        shownTime = time;
        shownScene = controller.currentSlide?.sceneId;
        return seek(time);
      }
      if (time === shownTime) return;

      const previousTime = shownTime;
      const previousScene = shownScene;
      stopEntrance();
      shownScene = controller.currentSlide?.sceneId;
      shownTime = time;
      seek(time);

      const slide = controller.currentSlide;
      if (matchMedia('(prefers-reduced-motion: reduce)').matches
        || !slide) return;

      const player = this.querySelector('hyperframes-player');
      const frame = player?.iframe
        || player?.querySelector('iframe')
        || player?.shadowRoot?.querySelector('iframe');
      const scene = frame?.contentDocument?.getElementById(`${slide.sceneId}-scene`);
      if (scene && slide.fragments.length) {
        const items = [...scene.querySelectorAll('.reveal-bullet,.reveal-option')];
        const newlyShown = items.filter((item, index) => previousScene === slide.sceneId
          && previousTime < slide.fragments[index + 1] && time >= slide.fragments[index + 1]);
        if (!newlyShown.length) return;
        targets = newlyShown.map(element => ({ element }));
        for (const element of newlyShown) element.style.opacity = '0';
        const generation = entranceGeneration;
        let started;
        const tick = now => {
          if (generation !== entranceGeneration) return;
          started ??= now;
          const progress = clamp((now - started) / 350);
          for (const element of newlyShown) element.style.opacity = String(easeOut(progress));
          if (progress < 1) entranceFrame = requestAnimationFrame(tick);
          else stopEntrance();
        };
        entranceFrame = requestAnimationFrame(tick);
        return;
      }
      if (time !== controller.restFrame(slide)) return;
      const mode = scene?.dataset.animation;
      if (!scene || !mode || mode === 'none') return;

      const heading = scene.querySelector('.heading .motion');
      const body = scene.querySelector('.body .motion');
      const pictures = scene.querySelectorAll('.slide-image');
      if (!heading && !body && !pictures.length) return;

      const add = (element, duration, delay, distance = 0, zoom = false) => {
        if (!element) return;
        targets.push({ element, duration, delay, distance, zoom });
        element.style.opacity = '0';
        if (zoom) element.style.transform = 'scale3d(.96,.96,1)';
        else if (distance) element.style.transform = `translate3d(0,${distance}px,0)`;
      };

      add(heading, 720, 0, mode === 'fade' || mode === 'zoom' ? 0 : 20, mode === 'zoom');
      add(body, 940, 110, mode === 'fade' ? 12 : 24);
      for (const picture of pictures) add(picture, 720, 0);
      if (!targets.length) return;

      const generation = entranceGeneration;
      const lastFrame = Math.max(...targets.map(target => target.delay + target.duration));
      let started = null;
      this.dataset.hfTextEntranceActive = 'true';
      const tick = now => {
        if (generation !== entranceGeneration) return;
        if (started === null) started = now;
        const elapsed = now - started;
        for (const target of targets) {
          const progress = clamp((elapsed - target.delay) / target.duration);
          const eased = target.distance || target.zoom ? easeOut(progress) : progress;
          target.element.style.opacity = String(easeOut(progress));
          if (target.zoom) {
            const scale = .96 + .04 * eased;
            target.element.style.transform = `scale3d(${scale},${scale},1)`;
          } else if (target.distance) {
            target.element.style.transform = `translate3d(0,${target.distance * (1 - eased)}px,0)`;
          }
        }
        if (elapsed < lastFrame) {
          entranceFrame = requestAnimationFrame(tick);
        } else {
          stopEntrance();
        }
      };
      entranceFrame = requestAnimationFrame(tick);
    };

    const result = bindController.call(this, controller);
    this.outlineNavigationCleanup?.();
    const player = this.querySelector('hyperframes-player');
    const frame = player?.iframe
      || player?.querySelector('iframe')
      || player?.shadowRoot?.querySelector('iframe');
    const document = frame?.contentDocument;
    if (document) {
      const navigate = event => {
        const button = event.target?.closest?.('.deck-outline .outline-jump');
        if (!button) {
          const scene = event.target?.closest?.('.slide');
          if (!scene || event.target.closest('.deck-outline,button,a,input,textarea,select')) return;
          if (event.type === 'click' && event.button !== 0) return;
          if (event.type === 'keydown' && event.key !== ' ') return;
          event.preventDefault();
          event.stopImmediatePropagation();
          controller.next();
          this.focus({ preventScroll: true });
          return;
        }
        if (event.type === 'keydown' && event.key !== 'Enter' && event.key !== ' ') return;
        const index = Number(button.dataset.outlineSlide);
        if (!Number.isInteger(index) || index < 0 || index >= controller.counter.total) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        controller.goToSlide(index);
        this.focus({ preventScroll: true });
      };
      document.addEventListener('click', navigate, true);
      document.addEventListener('keydown', navigate, true);
      this.outlineNavigationCleanup = () => {
        document.removeEventListener('click', navigate, true);
        document.removeEventListener('keydown', navigate, true);
      };
    }
    return result;
  };
})();
