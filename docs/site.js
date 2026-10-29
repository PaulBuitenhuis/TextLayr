/* TextLayr landing page — progressive enhancement only.
   With JavaScript off the page still works: the download buttons point at the
   version-agnostic asset aliases (releases/latest/download/TextLayr.dmg,
   …/TextLayr-linux-x86_64.AppImage and …/TextLayr-linux-x86_64.tar.xz, all
   published by .github/workflows/release.yml) and the static copy is used
   everywhere. With JS on we resolve the exact versioned assets instead and show
   their version and sizes; RELEASES below is the last-resort fallback if a
   release somehow carries no matching asset. */
(function () {
  'use strict';

  var REPO = 'PaulBuitenhuis/TextLayr';
  var API = 'https://api.github.com/repos/' + REPO + '/releases/latest';
  var RELEASES = 'https://github.com/' + REPO + '/releases/latest';
  var CACHE_KEY = 'textlayr-latest-release-v2';
  var CACHE_TTL = 30 * 60 * 1000; // 30 minutes

  var $ = function (sel) { return Array.prototype.slice.call(document.querySelectorAll(sel)); };
  var root = document.documentElement;

  /* ── Theme toggle ────────────────────────────────────────────────── */

  var toggle = document.querySelector('.theme-toggle');
  if (toggle) {
    toggle.addEventListener('click', function () {
      var current = root.dataset.theme;
      if (current !== 'light' && current !== 'dark') {
        current = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
      }
      var next = current === 'light' ? 'dark' : 'light';
      root.dataset.theme = next;
      try { localStorage.setItem('textlayr-theme', next); } catch (e) {}
    });
  }

  /* ── Copy-to-clipboard ───────────────────────────────────────────── */

  $('[data-copy]').forEach(function (btn) {
    btn.addEventListener('click', function () {
      var block = btn.closest('.code');
      var source = block && block.querySelector('code');
      if (!source) return;
      // innerText keeps <br> breaks as newlines; textContent would glue the
      // lines together and paste a non-command.
      var text = (source.innerText || source.textContent || '').trim();

      var done = function () {
        btn.classList.add('is-copied');
        var label = btn.querySelector('.copy-label');
        if (label) label.textContent = 'Copied';
        window.setTimeout(function () {
          btn.classList.remove('is-copied');
          if (label) label.textContent = 'Copy';
        }, 1800);
      };

      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done, function () { fallbackCopy(text, done); });
      } else {
        fallbackCopy(text, done);
      }
    });
  });

  function fallbackCopy(text, done) {
    var ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.top = '-1000px';
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); done(); } catch (e) { /* ignore */ }
    document.body.removeChild(ta);
  }

  /* ── Hero preview clip ───────────────────────────────────────────── */

  var video = document.getElementById('demo');
  var phone = video && video.closest('.phone');
  var playBtn = document.querySelector('[data-play-toggle]');
  var reduced = window.matchMedia('(prefers-reduced-motion: reduce)');

  if (video && playBtn) {
    playBtn.hidden = false;

    var sync = function () {
      if (phone) phone.classList.toggle('is-playing', !video.paused);
    };

    playBtn.addEventListener('click', function () {
      if (video.paused) {
        video.play().catch(function () {});
      } else {
        video.pause();
      }
    });

    video.addEventListener('play', sync);
    video.addEventListener('pause', sync);

    if (reduced.matches) {
      // Respect reduced motion: show the poster frame, let the visitor opt in.
      if (phone) phone.classList.add('is-playing');
      playBtn.style.opacity = '1';
    } else {
      video.addEventListener('canplay', function () {
        video.play().catch(function () { sync(); });
      }, { once: true });
      video.load();
    }
  }

  /* ── Newest release (version, sizes, direct downloads) ─────────────── */

  function formatSize(bytes) {
    if (!bytes) return '';
    var mb = bytes / 1048576;
    return mb >= 100 ? Math.round(mb) + ' MB' : mb.toFixed(1) + ' MB';
  }

  function readCache() {
    try {
      var raw = localStorage.getItem(CACHE_KEY);
      if (!raw) return null;
      var parsed = JSON.parse(raw);
      if (!parsed || (Date.now() - parsed.t) > CACHE_TTL) return null;
      return parsed.d;
    } catch (e) { return null; }
  }

  function writeCache(data) {
    try { localStorage.setItem(CACHE_KEY, JSON.stringify({ t: Date.now(), d: data })); } catch (e) {}
  }

  function asset(rel, pattern) {
    var found = (rel.assets || []).filter(function (a) { return pattern.test(a.name); })[0];
    return found
      ? { name: found.name, size: found.size, url: found.browser_download_url }
      : { name: '', size: 0, url: '' };
  }

  function apply(info) {
    var version = info.version || '';
    var dmg = info.dmg || {};
    var appimage = info.appimage || {};
    var tarball = info.tarball || {};

    $('[data-version]').forEach(function (el) { el.textContent = version || '—'; });
    $('[data-version-inline]').forEach(function (el) { el.textContent = version || 'latest'; });
    $('[data-size]').forEach(function (el) { el.textContent = formatSize(dmg.size) || 'free'; });
    $('[data-size-appimage]').forEach(function (el) { el.textContent = formatSize(appimage.size) || 'AppImage'; });
    $('[data-size-tarball]').forEach(function (el) { el.textContent = formatSize(tarball.size) || 'tarball'; });
    $('[data-file]').forEach(function (el) { el.textContent = dmg.name || 'TextLayr for macOS'; });
    $('[data-download]').forEach(function (el) { el.href = dmg.url || RELEASES; });
    $('[data-download-linux]').forEach(function (el) { el.href = appimage.url || RELEASES; });
    $('[data-download-tarball]').forEach(function (el) { el.href = tarball.url || RELEASES; });
    $('[data-release-meta]').forEach(function (el) { el.hidden = false; });
    $('[data-release-fallback]').forEach(function (el) { el.hidden = true; });
  }

  function fromRelease(rel) {
    return {
      version: String(rel.tag_name || rel.name || '').replace(/^v/, ''),
      dmg: asset(rel, /\.dmg$/i),
      appimage: asset(rel, /x86_64\.AppImage$/i),
      tarball: asset(rel, /x86_64\.tar\.xz$/i)
    };
  }

  var cached = readCache();
  if (cached) {
    apply(cached);
  } else if (window.fetch) {
    fetch(API, { headers: { Accept: 'application/vnd.github+json' } })
      .then(function (res) {
        if (!res.ok) throw new Error('HTTP ' + res.status);
        return res.json();
      })
      .then(function (rel) {
        var info = fromRelease(rel);
        writeCache(info);
        apply(info);
      })
      .catch(function () { /* keep the static copy and Releases fallback */ });
  }
})();

/* ── Style gallery ─────────────────────────────────────────────────────────
   Composes each built-in caption profile over the still frame, the same way
   the app composites over video. The look itself (fonts, colours, box, pill,
   motion) is CSS in style.css, keyed off [data-style]; this file only walks
   the words so the active-word highlight actually moves.

   GROUPING mirrors words-per-popup in assets/profiles/*.json. A landscape
   frame is shown with two extra words and larger type, the way you would
   configure it for a wider screen, so the preview is not just a narrower
   portrait. TEXT supplies preview copy sized to each grouping. */
(function () {
  'use strict';

  var stage = document.querySelector('[data-stage]');
  if (!stage) return;

  var line = stage.querySelector('[data-cap-line]');
  var buttons = Array.prototype.slice.call(document.querySelectorAll('.style-btn[data-style]'));
  var ratios = Array.prototype.slice.call(document.querySelectorAll('.ratio-toggle [data-ratio]'));
  var reduce = window.matchMedia('(prefers-reduced-motion: reduce)');

  var GROUPING = {
    'classic': 3,
    'bold-pop': 3,
    'clean-subtitle': 5,
    'minimal': 4,
    'highlight-pill': 3,
    'editorial': 3,
    'word-by-word': 1,
    'lower-third': 6
  };

  var TEXT = {
    1: 'every word lights up',
    3: 'every word lights',
    4: 'every word lights up',
    5: 'watch every word light up',
    6: 'watch every single word land'
  };

  var ratio = '16:9';
  var current = 'lower-third';

  var WORD_MS = 430;
  var GROUP_MS = 720;

  var groups = [];
  var groupIndex = 0;
  var wordIndex = 0;
  var timer = 0;
  var onScreen = true;

  function split(text, size) {
    var words = text.split(' ');
    var out = [];
    for (var i = 0; i < words.length; i += size) out.push(words.slice(i, i + size));
    return out;
  }

  function paint(enter) {
    var words = groups[groupIndex] || [];
    if (!words.length) return;

    line.textContent = '';
    words.forEach(function (word, i) {
      var span = document.createElement('span');
      span.className = 'cap-w' + (i === wordIndex ? ' is-on' : '');
      span.textContent = word;
      line.appendChild(span);
      if (i < words.length - 1) line.appendChild(document.createTextNode(' '));
    });

    if (enter) {
      line.classList.remove('is-enter');
      void line.offsetWidth; // restart the keyframe animation
      line.classList.add('is-enter');
    }
  }

  function schedule(ms) {
    window.clearTimeout(timer);
    timer = 0;
    if (!onScreen || reduce.matches) return;
    timer = window.setTimeout(step, ms);
  }

  function step() {
    timer = 0;
    var current = groups[groupIndex] || [];

    if (wordIndex + 1 < current.length) {
      wordIndex += 1;
      paint(false);
      schedule(WORD_MS);
    } else {
      var next = (groupIndex + 1) % groups.length;
      // A single-group profile (5 words at 5 per popup) never changes group,
      // so replaying the entrance animation would just blink the line.
      var changed = next !== groupIndex;
      wordIndex = 0;
      groupIndex = next;
      paint(changed);
      schedule(GROUP_MS);
    }
  }

  function confFor(id) {
    var words = GROUPING[id] || 3;
    // Word by Word stays one word at a time whatever the frame.
    if (ratio === '16:9' && words > 1) words = Math.min(words + 2, 6);
    return { words: words, text: TEXT[words] || TEXT[3] };
  }

  function apply(id, animate) {
    if (!GROUPING[id]) return;
    var conf = confFor(id);

    current = id;
    stage.setAttribute('data-style', id);
    groups = split(conf.text, conf.words);
    groupIndex = 0;
    wordIndex = 0;
    paint(animate);

    buttons.forEach(function (btn) {
      btn.setAttribute('aria-pressed', String(btn.getAttribute('data-style') === id));
    });

    schedule(GROUP_MS);
  }

  buttons.forEach(function (btn) {
    btn.addEventListener('click', function () {
      apply(btn.getAttribute('data-style'), true);
    });
  });

  ratios.forEach(function (btn) {
    btn.addEventListener('click', function () {
      ratio = btn.getAttribute('data-ratio');
      stage.setAttribute('data-ratio', ratio);
      ratios.forEach(function (other) {
        other.setAttribute('aria-pressed', String(other === btn));
      });
      // Grouping and copy change with the frame, so rebuild the line.
      apply(current, false);
    });
  });

  if ('IntersectionObserver' in window) {
    new IntersectionObserver(function (entries) {
      onScreen = entries[0].isIntersecting;
      if (onScreen) schedule(WORD_MS);
      else window.clearTimeout(timer);
    }, { threshold: 0.25 }).observe(stage);
  }

  document.addEventListener('visibilitychange', function () {
    if (document.hidden) window.clearTimeout(timer);
    else schedule(WORD_MS);
  });

  var onMotionChange = function () {
    if (reduce.matches) window.clearTimeout(timer);
    else schedule(WORD_MS);
  };
  if (reduce.addEventListener) reduce.addEventListener('change', onMotionChange);
  else if (reduce.addListener) reduce.addListener(onMotionChange);

  apply('lower-third', false);
})();
