/* Scrolling effects: a progress bar under the header, the cards, titles, figures and recordings fading in as they
   come into view, and the home page's banner drifting as it leaves. Plain IntersectionObserver and one scroll
   listener, no library. Nothing is hidden unless this ran, and nothing moves for visitors who ask for less motion. */
(() => {
  const calm = matchMedia("(prefers-reduced-motion: reduce)");
  const TARGETS = [
    ".md-typeset .grid.cards > ul > li",
    ".md-typeset h2", ".md-typeset h3",
    ".md-typeset .admonition", ".md-typeset details",
    ".md-typeset .asciicast", ".md-typeset img:not(.twemoji)", ".md-typeset .highlight", ".md-typeset table",
  ].join(",");

  let bar = null, observer = null, hero = null, ticking = false;
  const waiting = new Set();   // watched, not shown yet

  function progress() {
    if (!bar) {
      bar = document.createElement("div");
      bar.className = "scroll-progress";
      bar.setAttribute("aria-hidden", "true");
      document.body.append(bar);
    }
  }

  function frame() {
    ticking = false;
    const doc = document.documentElement;
    const room = doc.scrollHeight - doc.clientHeight;
    if (bar) bar.style.transform = `scaleX(${room > 0 ? Math.min(1, window.scrollY / room) : 0})`;
    // A jump (an anchor, the End key) can carry an element from below the screen to above it without the observer
    // ever seeing it on screen: show those too.
    for (const el of waiting) {
      if (el.offsetParent !== null && el.getBoundingClientRect().bottom < 0) show(el);
    }
    if (hero && hero.isConnected) {
      const h = hero.offsetHeight + hero.offsetTop || 1;
      const k = Math.min(1, Math.max(0, window.scrollY / h));
      doc.style.setProperty("--hero-shift", `${(k * 60).toFixed(1)}px`);
      doc.style.setProperty("--hero-fade", (1 - k * 0.8).toFixed(3));
    }
  }
  const onScroll = () => { if (!ticking) { ticking = true; requestAnimationFrame(frame); } };

  function show(el) {
    el.classList.add("in");
    waiting.delete(el);
    if (observer) observer.unobserve(el);
  }

  function init() {
    if (observer) observer.disconnect();
    waiting.clear();
    hero = document.querySelector(".custom-hero");
    if (calm.matches || !("IntersectionObserver" in window)) { document.documentElement.classList.remove("reveal-on"); frame(); return; }
    document.documentElement.classList.add("reveal-on");
    observer = new IntersectionObserver((entries) => {
      for (const e of entries) {
        // Above the screen (an anchor jumped past it): show it at once, there is nothing to watch appear.
        if (e.isIntersecting || e.boundingClientRect.top < 0) show(e.target);
      }
    }, { threshold: 0.05 });
    let row = 0, last = -1;
    for (const el of document.querySelectorAll(TARGETS)) {
      if (el.closest(".md-sidebar, .md-header, .md-tabs") || el.classList.contains("reveal")) continue;
      // Cards of one row come in one after the other.
      const top = Math.round(el.getBoundingClientRect().top + window.scrollY);
      row = top === last ? row + 1 : 0; last = top;
      el.style.setProperty("--reveal-delay", `${Math.min(row, 4) * 90}ms`);
      el.classList.add("reveal");
      observer.observe(el);
      waiting.add(el);
    }
    progress(); frame();
  }

  addEventListener("scroll", onScroll, { passive: true });
  addEventListener("resize", onScroll, { passive: true });
  if (window.document$) window.document$.subscribe(init);
  else document.addEventListener("DOMContentLoaded", init);
})();
