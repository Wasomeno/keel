// Page-world init script: console messages and page errors go to Keel's
// per-tab ring buffer through the `ipc` message handler. Text is capped and
// the rate limited here; Keel caps again on its side.
(function () {
  if (window.__keelConsole) return;
  try { Object.defineProperty(window, '__keelConsole', { value: true }); } catch (e) { return; }
  var h = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc;
  if (!h) return;
  var post = h.postMessage.bind(h), sent = 0, windowStart = Date.now();
  var frame = window === window.top ? '' : (location.host || 'frame');
  function str(v) {
    if (typeof v === 'string') return v;
    if (v instanceof Error) return v.stack || String(v);
    try { var j = JSON.stringify(v); return j === undefined ? String(v) : j; } catch (e) { return String(v); }
  }
  function send(level, text) {
    var now = Date.now();
    if (now - windowStart > 1000) { windowStart = now; sent = 0; }
    if (++sent > 50) return;
    text = String(text);
    if (text.length > 1000) text = text.slice(0, 1000) + '…';
    try { post(JSON.stringify({ k: 'keel-console', level: level, text: text, frame: frame })); } catch (e) {}
  }
  ['log', 'info', 'warn', 'error', 'debug'].forEach(function (level) {
    var orig = console[level];
    if (typeof orig !== 'function') return;
    console[level] = function () {
      try { send(level === 'warn' ? 'warning' : level, Array.prototype.map.call(arguments, str).join(' ')); } catch (e) {}
      return orig.apply(this, arguments);
    };
  });
  window.addEventListener('error', function (e) {
    if (e.target && e.target !== window && e.target.tagName) {
      send('error', 'failed to load ' + e.target.tagName.toLowerCase() + ' ' + (e.target.src || e.target.href || ''));
    } else {
      send('error', 'uncaught ' + (e.message || 'error') + (e.filename ? ' at ' + e.filename + ':' + e.lineno + ':' + e.colno : ''));
    }
  }, true);
  window.addEventListener('unhandledrejection', function (e) {
    send('error', 'unhandled promise rejection: ' + str(e.reason));
  });
})();
