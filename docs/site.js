/* TextLayr landing page — progressive enhancement only.
   With JavaScript off the page still works: the download buttons point at the
   version-agnostic asset alias (releases/latest/download/TextLayr.dmg, published
   by .github/workflows/release.yml) and the static copy is used everywhere.
   With JS on we resolve the exact versioned asset instead and show its version
   and size; RELEASES below is the last-resort fallback if a release somehow
   carries no .dmg. */
(function () {
  'use strict';

  var REPO = 'PaulBuitenhuis/TextLayr';
  var API = 'https://api.github.com/repos/' + REPO + '/releases/latest';
  var RELEASES = 'https://github.com/' + REPO + '/releases/latest';
  var CACHE_KEY = 'textlayr-latest-release';
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
      var text = source.textContent.trim();

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

  /* ── Newest release (version, size, direct download) ─────────────── */

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

  function apply(info) {
    var version = info.version || '';
    var size = formatSize(info.size);

    $('[data-version]').forEach(function (el) { el.textContent = version || '—'; });
    $('[data-version-inline]').forEach(function (el) { el.textContent = version || 'latest'; });
    $('[data-size]').forEach(function (el) { el.textContent = size || 'free'; });
    $('[data-file]').forEach(function (el) { el.textContent = info.name || 'TextLayr for macOS'; });
    $('[data-download]').forEach(function (el) { el.href = info.url || RELEASES; });
    $('[data-release-meta]').forEach(function (el) { el.hidden = false; });
    $('[data-release-fallback]').forEach(function (el) { el.hidden = true; });
  }

  function fromRelease(rel) {
    var dmg = (rel.assets || []).filter(function (a) { return /\.dmg$/i.test(a.name); })[0];
    return {
      version: String(rel.tag_name || rel.name || '').replace(/^v/, ''),
      name: dmg ? dmg.name : '',
      size: dmg ? dmg.size : 0,
      url: dmg ? dmg.browser_download_url : RELEASES
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
