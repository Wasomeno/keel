/* ==========================================================================
   Keel Landing Page — Smooth Scroll, Interactive Demos & 3D Bot Controls
   ========================================================================== */

(function () {
  'use strict';

  var INSTALL_CMD = 'curl -fsSL https://raw.githubusercontent.com/Wasomeno/keel/main/install.sh | sh';
  var reducedMotion = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  /* 1. Theme Toggle */
  var themeToggle = document.getElementById('themeToggle');
  if (themeToggle) {
    themeToggle.addEventListener('click', function () {
      var html = document.documentElement;
      var next = html.getAttribute('data-theme') === 'dark' ? 'light' : 'dark';
      html.setAttribute('data-theme', next);
      try { localStorage.setItem('keel-theme', next); } catch (e) {}
      if (window.KeelCreatures && window.KeelCreatures.redrawStatic) {
        window.KeelCreatures.redrawStatic();
      }
    });
  }

  /* 2. Silky Smooth Inertial Scroll Effect (Desktop wheel + anchor links) */
  var smoothScroll = (function () {
    var currentY = window.scrollY || 0;
    var targetY = currentY;
    var running = false;
    var ease = 0.095;

    function maxScroll() {
      return Math.max(document.documentElement.scrollHeight - window.innerHeight, 0);
    }

    function step() {
      var diff = targetY - currentY;
      if (Math.abs(diff) < 0.35) {
        currentY = targetY;
        window.scrollTo(0, Math.round(currentY));
        running = false;
        return;
      }
      currentY += diff * ease;
      window.scrollTo(0, currentY);
      requestAnimationFrame(step);
    }

    function start() {
      if (!running) {
        running = true;
        requestAnimationFrame(step);
      }
    }

    function scrollToY(y) {
      if (reducedMotion) {
        window.scrollTo(0, y);
        return;
      }
      currentY = window.scrollY || 0;
      targetY = Math.max(0, Math.min(maxScroll(), y));
      start();
    }

    if (!reducedMotion && window.matchMedia('(pointer: fine)').matches) {
      window.addEventListener('wheel', function (e) {
        if (e.ctrlKey || e.metaKey) return;
        // Allow horizontal scrolling inside code blocks
        var target = e.target;
        if (target && target.closest && target.closest('.command-bar code')) return;

        e.preventDefault();
        if (!running) {
          currentY = window.scrollY || 0;
          targetY = currentY;
        }
        var delta = e.deltaY;
        if (e.deltaMode === 1) delta *= 36;
        else if (e.deltaMode === 2) delta *= window.innerHeight;
        targetY = Math.max(0, Math.min(maxScroll(), targetY + delta * 0.92));
        start();
      }, { passive: false });

      // Keep synced if user drags scrollbar or uses keyboard
      window.addEventListener('scroll', function () {
        if (!running) {
          currentY = window.scrollY || 0;
          targetY = currentY;
        }
      }, { passive: true });
    }

    // Smooth anchor navigation (#install, #top, etc.)
    document.querySelectorAll('a[href^="#"]').forEach(function (link) {
      link.addEventListener('click', function (e) {
        var id = link.getAttribute('href');
        if (!id || id === '#') return;
        if (id === '#top') {
          e.preventDefault();
          scrollToY(0);
          return;
        }
        var dest = document.querySelector(id);
        if (dest) {
          e.preventDefault();
          var y = dest.getBoundingClientRect().top + (window.scrollY || 0) - 68;
          scrollToY(y);
        }
      });
    });

    return { scrollToY: scrollToY };
  })();

  /* 3. Copy Install Command */
  function copyText(text, onDone) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(onDone, function () { fallbackCopy(text, onDone); });
    } else {
      fallbackCopy(text, onDone);
    }
  }

  function fallbackCopy(text, cb) {
    var area = document.createElement('textarea');
    area.value = text;
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    try { document.execCommand('copy'); cb(); } catch (e) {}
    document.body.removeChild(area);
  }

  var copyBtn = document.getElementById('copyInstallBtn');
  var copyLabel = document.getElementById('copyInstallText');
  if (copyBtn) {
    copyBtn.addEventListener('click', function () {
      copyText(INSTALL_CMD, function () {
        copyBtn.classList.add('is-copied');
        if (copyLabel) copyLabel.textContent = 'Copied!';
        setTimeout(function () {
          copyBtn.classList.remove('is-copied');
          if (copyLabel) copyLabel.textContent = 'Copy';
        }, 2000);
      });
    });
  }

  /* 4. Single Interactive 1:1 Keel Window + Sticky Scroll Showcase */
  var keelWindow = document.getElementById('keelWindow');
  var heroScrollTrack = document.getElementById('heroScrollTrack');
  var scenePanels = document.querySelectorAll('[data-scene-panel]');
  var browserPane = document.getElementById('kwBrowserPane');
  var panelToggleBtn = document.getElementById('kwPanelToggle');
  var browserCloseBtn = document.getElementById('kwBrowserClose');
  var modelText = document.getElementById('kwModelText');
  var mainCrumb = document.getElementById('kwMainCrumb');
  var kwTriggers = document.querySelectorAll('[data-kw-trigger]');
  var railItems = document.querySelectorAll('.showcase-rail .sr-item');

  var sceneInfo = {
    chat: { model: 'Claude Sonnet · High', crumb: 'Spaces / personal' },
    bots: { model: 'Dr. Keelee · Auto', crumb: 'Bots / Dr. Keelee' },
    browser: { model: 'ChatGPT · Medium', crumb: 'Spaces / personal · Browser' }
  };

  var currentScene = 'chat';
  var manualOverrideUntil = 0;

  function setKeelScene(target, opts) {
    if (!target || !sceneInfo[target]) return;
    var isManual = opts && opts.manual;
    if (isManual) {
      manualOverrideUntil = performance.now() + 900;
    }
    if (currentScene === target && !(opts && opts.force)) return;
    currentScene = target;

    var openBrowser = target === 'browser';

    if (keelWindow) {
      keelWindow.setAttribute('data-active-scene', target);
      keelWindow.classList.toggle('browser-open', openBrowser);
    }

    if (browserPane) {
      browserPane.setAttribute('aria-hidden', openBrowser ? 'false' : 'true');
    }

    if (panelToggleBtn) {
      panelToggleBtn.classList.toggle('is-active', openBrowser);
    }

    scenePanels.forEach(function (panel) {
      var active = panel.getAttribute('data-scene-panel') === target;
      panel.classList.toggle('is-active', active);
      panel.setAttribute('aria-hidden', active ? 'false' : 'true');
    });

    // Update sidebar active selection + bottom hairline rail
    kwTriggers.forEach(function (el) {
      var match = el.getAttribute('data-kw-trigger') === target;
      if (el.classList.contains('sr-item')) {
        el.classList.toggle('is-active', match);
        if (isManual) {
          var f = el.querySelector('.sr-fill');
          if (f) f.style.transform = '';
        }
      } else {
        el.classList.toggle('is-selected', match);
      }
    });

    if (modelText) modelText.textContent = sceneInfo[target].model;
    if (mainCrumb) mainCrumb.textContent = sceneInfo[target].crumb;
  }

  // Click directly inside the Keel sidebar or subtle hairline rail
  kwTriggers.forEach(function (el) {
    el.addEventListener('click', function () {
      var target = el.getAttribute('data-kw-trigger');
      setKeelScene(target, { manual: true, force: true });
    });
  });

  // Real top-right panel toggle button inside the Keel window
  if (panelToggleBtn) {
    panelToggleBtn.addEventListener('click', function () {
      var next = currentScene === 'browser' ? 'chat' : 'browser';
      setKeelScene(next, { manual: true, force: true });
    });
  }

  // Close (×) button inside the Browser right-panel strip
  if (browserCloseBtn) {
    browserCloseBtn.addEventListener('click', function () {
      setKeelScene('chat', { manual: true, force: true });
    });
  }

  setKeelScene('chat', { force: true });

  // Drive sticky scroll beats smoothly as the user scrolls through #heroScrollTrack
  function updateHeroScrollShowcase() {
    if (reducedMotion || !heroScrollTrack || window.innerWidth <= 960) return;
    if (performance.now() < manualOverrideUntil) return;

    var rect = heroScrollTrack.getBoundingClientRect();
    var scrollable = heroScrollTrack.offsetHeight - window.innerHeight + 78;
    if (scrollable <= 60) return;

    var progress = Math.max(0, Math.min(1, (-rect.top + 78) / scrollable));

    // Update subtle hairline fill bars proportionally while scrolling
    var idx = progress < 0.36 ? 0 : (progress < 0.70 ? 1 : 2);
    var scenes = ['chat', 'bots', 'browser'];
    setKeelScene(scenes[idx]);

    railItems.forEach(function (item, i) {
      var fill = item.querySelector('.sr-fill');
      if (!fill) return;
      var segStart = i === 0 ? 0 : (i === 1 ? 0.36 : 0.70);
      var segEnd = i === 0 ? 0.36 : (i === 1 ? 0.70 : 1.0);
      var localP = Math.max(0, Math.min(1, (progress - segStart) / (segEnd - segStart)));
      if (i < idx) localP = 1;
      if (i > idx) localP = 0;
      fill.style.transform = 'scaleX(' + (i === idx ? Math.max(0.18, localP) : localP).toFixed(3) + ')';
    });
  }

  /* 5. Interactive Browser Control HUD ("Take over" / "Hand back") */
  document.querySelectorAll('[data-hud-toggle]').forEach(function (btn) {
    btn.addEventListener('click', function () {
      var hud = btn.closest('.rp-hud');
      if (!hud) return;
      var text = hud.querySelector('.hud-text');
      var isAgent = hud.getAttribute('data-hud-state') !== 'user';
      if (isAgent) {
        hud.setAttribute('data-hud-state', 'user');
        if (text) text.textContent = "You're in control";
        btn.textContent = 'Hand back';
      } else {
        hud.setAttribute('data-hud-state', 'agent');
        if (text) text.textContent = 'Keel is browsing';
        btn.textContent = 'Take over';
      }
    });
  });

  /* 6. Animated Hub-and-Spoke Diagram */
  var hubNodes = Array.prototype.slice.call(document.querySelectorAll('.hub-node'));
  var hubWires = document.querySelectorAll('.wire');
  var hubStreams = document.querySelectorAll('.wire-stream');
  var hubCaptionText = document.getElementById('hubCaptionText');
  var hubCycleIdx = 0;
  var hubPaused = false;

  function highlightHubNode(node) {
    var key = node.getAttribute('data-node');
    var caption = node.getAttribute('data-caption');

    hubNodes.forEach(function (n) { n.classList.toggle('is-highlighted', n === node); });
    hubWires.forEach(function (w) { w.classList.toggle('is-active', w.getAttribute('data-wire') === key); });
    hubStreams.forEach(function (s) { s.classList.toggle('is-active', s.getAttribute('data-stream') === key); });
    if (hubCaptionText && caption) hubCaptionText.textContent = caption;
  }

  hubNodes.forEach(function (node, idx) {
    node.addEventListener('click', function () {
      hubPaused = true;
      hubCycleIdx = idx;
      highlightHubNode(node);
    });
    node.addEventListener('mouseenter', function () {
      if (window.matchMedia('(hover: hover) and (pointer: fine)').matches) {
        hubPaused = true;
        hubCycleIdx = idx;
        highlightHubNode(node);
      }
    });
  });

  var hubDiagram = document.getElementById('hubDiagram');
  if (hubDiagram) {
    hubDiagram.addEventListener('mouseleave', function () { hubPaused = false; });
  }

  if (hubNodes.length) {
    highlightHubNode(hubNodes[0]);
    if (!reducedMotion) {
      setInterval(function () {
        if (hubPaused) return;
        hubCycleIdx = (hubCycleIdx + 1) % hubNodes.length;
        highlightHubNode(hubNodes[hubCycleIdx]);
      }, 3000);
    }
  }

  /* 7. Visual Answers Card Gallery + Scroll-Triggered Chart Bars */
  var cardTabs = document.querySelectorAll('.cp-tab');
  var cardPanels = document.querySelectorAll('[data-card-panel]');

  function animateChartBars() {
    document.querySelectorAll('#scrollChart .vbar-fill').forEach(function (bar) {
      bar.style.height = bar.getAttribute('data-height') || '60%';
    });
  }

  cardTabs.forEach(function (tab) {
    tab.addEventListener('click', function () {
      var target = tab.getAttribute('data-card');
      cardTabs.forEach(function (t) {
        var active = t === tab;
        t.classList.toggle('is-active', active);
        t.setAttribute('aria-selected', active ? 'true' : 'false');
      });
      cardPanels.forEach(function (p) {
        p.classList.toggle('is-hidden', p.getAttribute('data-card-panel') !== target);
      });
      if (target === 'chart') animateChartBars();
    });
  });

  /* 8. Interactive Checklist & Choices */
  var checklist = document.getElementById('interactiveChecklist');
  var stepsCounter = document.getElementById('stepsCounter');
  var stepsProgress = document.getElementById('stepsProgress');

  function updateChecklist() {
    if (!checklist) return;
    var rows = checklist.querySelectorAll('.check-row');
    var done = 0;
    rows.forEach(function (row) {
      var input = row.querySelector('input[type="checkbox"]');
      var checked = !!(input && input.checked);
      row.classList.toggle('is-done', checked);
      if (checked) done++;
    });
    if (stepsCounter) stepsCounter.textContent = done + ' of ' + rows.length;
    if (stepsProgress) stepsProgress.style.width = Math.round((done / rows.length) * 100) + '%';
  }
  if (checklist) {
    checklist.addEventListener('change', updateChecklist);
    updateChecklist();
  }

  var choicesWrap = document.getElementById('interactiveChoices');
  var choiceFeedback = document.getElementById('choiceFeedback');
  if (choicesWrap) {
    var btns = choicesWrap.querySelectorAll('.choice-card');
    btns.forEach(function (btn) {
      btn.addEventListener('click', function () {
        btns.forEach(function (b) { b.classList.toggle('is-selected', b === btn); });
        if (choiceFeedback) choiceFeedback.textContent = btn.getAttribute('data-reply') || '';
      });
    });
  }

  /* 9. Interactive 3D Bot Customizer (Color, Outfit, Hair, Eyes) */
  var studioCanvas = document.getElementById('studioCreature');

  function bindPicker(containerId, prop) {
    var wrap = document.getElementById(containerId);
    if (!wrap || !studioCanvas) return;
    var items = wrap.querySelectorAll('button[data-val]');
    items.forEach(function (btn) {
      btn.addEventListener('click', function () {
        items.forEach(function (b) { b.classList.toggle('is-active', b === btn); });
        var patch = {};
        patch[prop] = btn.getAttribute('data-val');
        if (window.KeelCreatures) {
          window.KeelCreatures.update(studioCanvas, patch);
        }
      });
    });
  }

  bindPicker('colorSwatches', 'color');
  bindPicker('attirePicker', 'attire');
  bindPicker('hairPicker', 'hair');
  bindPicker('eyesPicker', 'eyes');

  /* 10. Scroll Progress Hairline, Wave Reveals & Subtle Hero Depth */
  var progressBar = document.getElementById('scrollProgressBar');
  var scrollTicking = false;

  function onScrollFrame() {
    scrollTicking = false;
    var sy = window.scrollY || window.pageYOffset || 0;
    if (progressBar) {
      var max = Math.max(document.documentElement.scrollHeight - window.innerHeight, 1);
      progressBar.style.transform = 'scaleX(' + Math.min(sy / max, 1).toFixed(4) + ')';
    }
    updateHeroScrollShowcase();
  }

  window.addEventListener('scroll', function () {
    if (!scrollTicking) {
      scrollTicking = true;
      requestAnimationFrame(onScrollFrame);
    }
  }, { passive: true });
  onScrollFrame();

  if (!reducedMotion && 'IntersectionObserver' in window) {
    document.documentElement.classList.add('scroll-ready');

    var revealEls = document.querySelectorAll('[data-reveal]');
    var revealObs = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        var el = entry.target;
        var delay = parseInt(el.getAttribute('data-delay') || '0', 10);
        if (delay > 0) el.style.transitionDelay = delay + 'ms';
        el.classList.add('is-visible');
        revealObs.unobserve(el);
      });
    }, { threshold: 0.12, rootMargin: '0px 0px -8% 0px' });

    revealEls.forEach(function (el) { revealObs.observe(el); });

    var scrollChart = document.getElementById('scrollChart');
    if (scrollChart) {
      var chartObs = new IntersectionObserver(function (entries) {
        if (entries[0].isIntersecting) {
          setTimeout(animateChartBars, 160);
          chartObs.disconnect();
        }
      }, { threshold: 0.25 });
      chartObs.observe(scrollChart);
    }
    // Scroll-triggered RAM bars (GPU scaleX, staggered slightly for hierarchy)
    var ramInfographic = document.getElementById('ramInfographic');
    if (ramInfographic) {
      var ramObs = new IntersectionObserver(function (entries) {
        if (!entries[0].isIntersecting) return;
        var fills = ramInfographic.querySelectorAll('.ram-fill');
        fills.forEach(function (fill, i) {
          var s = fill.getAttribute('data-scale') || '0.5';
          setTimeout(function () {
            fill.style.transform = 'scaleX(' + s + ')';
          }, i * 110 + 120);
        });
        ramObs.disconnect();
      }, { threshold: 0.25 });
      ramObs.observe(ramInfographic);
    }
  } else {
    animateChartBars();
    document.querySelectorAll('#ramInfographic .ram-fill').forEach(function (fill) {
      fill.style.transform = 'scaleX(' + (fill.getAttribute('data-scale') || '0.5') + ')';
    });
  }
  /* 11. Single-Open Smooth FAQ Accordion */
  var faqItems = document.querySelectorAll('#faqList .faq-item');
  faqItems.forEach(function (item) {
    var btn = item.querySelector('.faq-q');
    if (!btn) return;
    btn.addEventListener('click', function () {
      var willOpen = !item.classList.contains('is-open');
      faqItems.forEach(function (other) {
        var otherBtn = other.querySelector('.faq-q');
        var open = other === item && willOpen;
        other.classList.toggle('is-open', open);
        if (otherBtn) otherBtn.setAttribute('aria-expanded', open ? 'true' : 'false');
      });
    });
  });
})();
