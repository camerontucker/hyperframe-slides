// HyperFrames normally seeks to the middle of each slide. Play its authored
// entrance instead, so the heading and body ease in while pictures fade in.
(() => {
  const Slideshow = customElements.get('hyperframes-slideshow');
  if (!Slideshow || !Slideshow.prototype.bindController) return;

  const bindController = Slideshow.prototype.bindController;
  Slideshow.prototype.bindController = function (controller) {
    const seek = controller.playTo.bind(controller);
    let shownTime = controller.currentSlide?.fragments?.[0]
      ?? (controller.currentSlide ? controller.restFrame(controller.currentSlide) : null);
    let firstAudienceSync = this.resolveMode() === 'audience';
    let entranceTimer = null;

    const stopEntrance = () => {
      if (entranceTimer !== null) {
        clearTimeout(entranceTimer);
        entranceTimer = null;
        controller.player.pause();
      }
      delete this.dataset.hfTextEntranceActive;
    };

    controller.playTo = (time) => {
      if (firstAudienceSync) {
        firstAudienceSync = false;
        stopEntrance();
        shownTime = time;
        return seek(time);
      }
      if (time === shownTime) return;

      stopEntrance();
      shownTime = time;
      const slide = controller.currentSlide;
      if (matchMedia('(prefers-reduced-motion: reduce)').matches
        || !slide || slide.fragments.length || time !== controller.restFrame(slide)) {
        return seek(time);
      }

      seek(slide.start + 0.01);
      this.dataset.hfTextEntranceActive = 'true';
      controller.player.play();
      entranceTimer = setTimeout(() => {
        entranceTimer = null;
        controller.player.pause();
        seek(time);
        delete this.dataset.hfTextEntranceActive;
      }, 1400);
    };

    return bindController.call(this, controller);
  };
})();
