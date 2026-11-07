// Console recordings: plays the asciicast files of docs/book/assets/demo/ with
// asciinema-player (loaded in zensical.toml). Real text, smooth and sharp at any
// size, pausable and seekable; a GIF of the same session is 10-50 times bigger.
//
// Markup, in a page:
//   <div class="asciicast" data-cast="../assets/demo/cli.cast"></div>
// Optional: data-speed, data-idle (seconds of silence kept), data-poster ("npt:5").
//
// Each player is created when it first scrolls into view, then plays in a loop.
(function () {
  function mount(el) {
    if (el.dataset.mounted) return;
    el.dataset.mounted = "1";
    const create = () =>
      AsciinemaPlayer.create(new URL(el.dataset.cast, document.baseURI).href, el, {
        autoPlay: true,
        loop: true,
        preload: true,
        fit: "width",
        speed: Number(el.dataset.speed || 1),
        idleTimeLimit: Number(el.dataset.idle || 1.5),
        poster: el.dataset.poster || "npt:0:3",
        theme: "monokai",
        terminalFontFamily: "'JetBrains Mono', ui-monospace, monospace",
      });
    if (!("IntersectionObserver" in window)) return create();
    const seen = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          seen.disconnect();
          create();
        }
      },
      { threshold: 0.2 },
    );
    seen.observe(el);
  }

  function init() {
    document.querySelectorAll(".asciicast[data-cast]").forEach(mount);
  }

  // `document$` fires on every page shown with instant navigation (Zensical /
  // Material); without it, a plain page load.
  if (window.document$) window.document$.subscribe(init);
  else document.addEventListener("DOMContentLoaded", init);
})();
