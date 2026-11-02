// Keel's agent library: the interactive snapshot with stable refs, and the
// actions that take them. Runs in Keel's own content world (pages can't see
// or tamper with it); the DOM is shared with the page.
//
// Built once per document as `window.__keelAgent`; every call returns a JSON
// string and never throws.
(function () {
  if (window.__keelAgent) return window.__keelAgent;

  // ---- refs ----------------------------------------------------------------
  // One registry for the whole page (same-origin iframes and shadow roots
  // included). A ref names one element for the life of the document: the
  // element keeps its ref across snapshots, numbers are never reused, and a
  // ref whose element left the page is reported stale, never re-pointed.
  var R = { next: 1, byRef: new Map(), byNode: new WeakMap() };

  function refOf(el, role) {
    var r = R.byNode.get(el);
    if (!r) {
      r = 'e' + R.next++;
      R.byNode.set(el, r);
    }
    R.byRef.set(r, { ref: new WeakRef(el), role: role });
    return r;
  }

  function staleRefs() {
    R.byRef.forEach(function (v, k) {
      var el = v.ref.deref();
      if (!el || !el.isConnected) R.byRef.delete(k);
    });
  }

  function normRef(s) {
    var m = /e\d+/.exec(String(s || ''));
    return m ? m[0] : String(s || '');
  }

  // The element `ref` names, re-validated: still on the page, still the same
  // kind of element. Throws a message that tells the agent what to do.
  function deref(ref) {
    ref = normRef(ref);
    var n = Number(ref.slice(1));
    var hit = R.byRef.get(ref);
    var el = hit && hit.ref.deref();
    if (!hit) {
      if (n > 0 && n < R.next)
        throw new Error('stale ref ' + ref + ': that element is gone from the page. Take a new browser_snapshot and use its refs');
      throw new Error('unknown ref ' + ref + ': refs come from browser_snapshot on the current page (it may have navigated since). Take a new snapshot');
    }
    if (!el || !el.isConnected) {
      R.byRef.delete(ref);
      throw new Error('stale ref ' + ref + ': that element is gone from the page. Take a new browser_snapshot and use its refs');
    }
    var role = roleOf(el, tagOf(el));
    if (hit.role && role && role !== hit.role && hit.role !== 'clickable')
      throw new Error('stale ref ' + ref + ': it was a ' + hit.role + ', now a ' + role + '. Take a new browser_snapshot');
    return el;
  }

  // ---- helpers -------------------------------------------------------------
  function txt(s) { return String(s == null ? '' : s).replace(/\s+/g, ' ').trim(); }
  function cut(s, n) { s = txt(s); return s.length > n ? s.slice(0, n - 1) + '…' : s; }
  function tagOf(el) { return el && el.tagName ? el.tagName.toLowerCase() : ''; }
  function win(el) { return (el && el.ownerDocument && el.ownerDocument.defaultView) || window; }
  function style(el) { try { return win(el).getComputedStyle(el); } catch (e) { return null; } }
  function inputType(el) { return String(el.type || 'text').toLowerCase(); }

  var LANDMARK = { banner: 1, navigation: 1, main: 1, complementary: 1, contentinfo: 1, form: 1, search: 1, region: 1, dialog: 1, alertdialog: 1 };
  var WIDGET = { button: 1, link: 1, checkbox: 1, radio: 1, 'switch': 1, tab: 1, menuitem: 1, menuitemcheckbox: 1,
    menuitemradio: 1, option: 1, combobox: 1, textbox: 1, searchbox: 1, slider: 1, spinbutton: 1, listbox: 1,
    treeitem: 1, file: 1 };
  // Widgets whose insides aren't listed (a button's spans, a select's options).
  var LEAF = { button: 1, link: 1, checkbox: 1, radio: 1, 'switch': 1, tab: 1, menuitem: 1, menuitemcheckbox: 1,
    menuitemradio: 1, option: 1, textbox: 1, searchbox: 1, slider: 1, spinbutton: 1, file: 1 };
  var ROLES = Object.assign({ heading: 1, alert: 1, status: 1, img: 0, none: 0, presentation: 0, generic: 0 }, LANDMARK, WIDGET);

  function inSection(el) {
    for (var p = el.parentElement; p; p = p.parentElement) {
      var t = tagOf(p);
      if (t === 'article' || t === 'section' || t === 'main' || t === 'aside' || t === 'nav') return true;
    }
    return false;
  }

  function implicitRole(el, tag) {
    switch (tag) {
      case 'a': case 'area': return el.hasAttribute('href') ? 'link' : '';
      case 'button': case 'summary': return 'button';
      case 'select': return el.multiple || el.size > 1 ? 'listbox' : 'combobox';
      case 'textarea': return 'textbox';
      case 'input':
        var ty = inputType(el);
        if (ty === 'hidden') return '';
        if (ty === 'button' || ty === 'submit' || ty === 'reset' || ty === 'image' || ty === 'color') return 'button';
        if (ty === 'checkbox' || ty === 'radio') return ty;
        if (ty === 'range') return 'slider';
        if (ty === 'number') return 'spinbutton';
        if (ty === 'search') return 'searchbox';
        if (ty === 'file') return 'file';
        return 'textbox';
      case 'h1': case 'h2': case 'h3': case 'h4': case 'h5': case 'h6': return 'heading';
      case 'nav': return 'navigation';
      case 'main': return 'main';
      case 'aside': return 'complementary';
      case 'header': return inSection(el) ? '' : 'banner';
      case 'footer': return inSection(el) ? '' : 'contentinfo';
      case 'form': return 'form';
      case 'dialog': return el.open ? 'dialog' : '';
      case 'section': return el.hasAttribute('aria-label') || el.hasAttribute('aria-labelledby') ? 'region' : '';
      case 'iframe': case 'frame': return 'iframe';
    }
    if (el.isContentEditable && !(el.parentElement && el.parentElement.isContentEditable)) return 'textbox';
    return '';
  }

  function roleOf(el, tag) {
    var explicit = txt(el.getAttribute && el.getAttribute('role')).split(' ')[0].toLowerCase();
    if (explicit && ROLES[explicit] === 1) return explicit;
    if (explicit && ROLES[explicit] === 0) return explicit === 'img' ? '' : '';
    return implicitRole(el, tag);
  }

  function byIds(el, ids) {
    var root = el.getRootNode ? el.getRootNode() : el.ownerDocument, out = [];
    ids.split(/\s+/).forEach(function (id) {
      var n = (root && root.getElementById && root.getElementById(id)) || el.ownerDocument.getElementById(id);
      if (n) out.push(n.innerText || n.textContent || '');
    });
    return txt(out.join(' '));
  }

  function nameOf(el, role, tag) {
    var v = el.getAttribute('aria-labelledby');
    if (v && (v = byIds(el, v))) return v;
    if ((v = txt(el.getAttribute('aria-label')))) return v;
    if (tag === 'input' || tag === 'textarea' || tag === 'select') {
      var ty = inputType(el);
      if (tag === 'input' && (ty === 'button' || ty === 'submit' || ty === 'reset'))
        return txt(el.value) || (ty === 'submit' ? 'Submit' : ty === 'reset' ? 'Reset' : '');
      if (tag === 'input' && ty === 'image') return txt(el.alt) || 'Submit';
      if (el.labels && el.labels.length) {
        v = txt(Array.prototype.map.call(el.labels, function (l) { return l.innerText || l.textContent; }).join(' '));
        if (v) return v;
      }
      return txt(el.getAttribute('placeholder')) || txt(el.getAttribute('title')) || txt(el.getAttribute('name'));
    }
    if (role === 'textbox' || role === 'searchbox' || role === 'combobox' || role === 'listbox' || role === 'slider' || role === 'spinbutton')
      return txt(el.getAttribute('aria-placeholder') || el.getAttribute('placeholder') || el.getAttribute('title'));
    if (role === 'dialog' || role === 'alertdialog') {
      var h = el.querySelector('h1,h2,h3,[role=heading]');
      return h ? cut(h.innerText || h.textContent, 80) : '';
    }
    if (LANDMARK[role]) return '';
    v = txt(el.innerText);
    if (!v) {
      var img = el.querySelector && el.querySelector('img[alt],[aria-label],[title]');
      if (img) v = txt(img.getAttribute('alt') || img.getAttribute('aria-label') || img.getAttribute('title'));
    }
    return v || txt(el.getAttribute('title'));
  }

  function valueOf(el, role, tag) {
    if (tag === 'input') {
      var ty = inputType(el);
      if (ty === 'file') return el.files && el.files.length ? Array.prototype.map.call(el.files, function (f) { return f.name; }).join(', ') : '';
      if (/^(checkbox|radio|button|submit|reset|image|color)$/.test(ty)) return '';
      if (ty === 'password') return el.value ? '••••' : '';
      return el.value;
    }
    if (tag === 'textarea') return el.value;
    if (tag === 'select')
      return Array.prototype.filter.call(el.options, function (o) { return o.selected; })
        .map(function (o) { return txt(o.label || o.text); }).join(', ');
    if (role === 'textbox' && el.isContentEditable) return el.innerText;
    var v = el.getAttribute('aria-valuetext') || el.getAttribute('aria-valuenow');
    return v == null ? '' : v;
  }

  function deepActive() {
    var a = document.activeElement;
    for (var i = 0; a && i < 20; i++) {
      if (a.shadowRoot && a.shadowRoot.activeElement) { a = a.shadowRoot.activeElement; continue; }
      if (tagOf(a) === 'iframe') { try { var d = a.contentDocument; if (d && d.activeElement) { a = d.activeElement; continue; } } catch (e) {} }
      break;
    }
    return a;
  }

  function statesOf(el, role, tag, active) {
    var s = [];
    if (/^(checkbox|radio|switch|menuitemcheckbox|menuitemradio)$/.test(role)) {
      if (tag === 'input' && el.indeterminate) s.push('mixed');
      else if (tag === 'input' ? el.checked : el.getAttribute('aria-checked') === 'true') s.push('checked');
    }
    if (el.getAttribute('aria-pressed') === 'true') s.push('pressed');
    var x = el.getAttribute('aria-expanded');
    if (x === 'true') s.push('expanded'); else if (x === 'false') s.push('collapsed');
    if (el.getAttribute('aria-selected') === 'true' || (tag === 'option' && el.selected)) s.push('selected');
    if (el.disabled || el.getAttribute('aria-disabled') === 'true') s.push('disabled');
    if ((tag === 'input' || tag === 'textarea') && el.readOnly) s.push('readonly');
    if (el.required || el.getAttribute('aria-required') === 'true') s.push('required');
    if (el.getAttribute('aria-invalid') === 'true') s.push('invalid');
    if (el === active) s.push('focused');
    if (role === 'heading') {
      var lv = el.getAttribute('aria-level') || (/^h([1-6])$/.exec(tag) || [])[1];
      if (lv) s.push('level=' + lv);
    }
    return s;
  }

  function hidden(el) {
    if (el.hidden) return true;
    if (el.getAttribute('aria-hidden') === 'true') return true;
    if (el.checkVisibility) return !el.checkVisibility({ visibilityProperty: true });
    var st = style(el);
    return !st || st.display === 'none' || st.visibility === 'hidden';
  }

  function sized(el, tag) {
    var r = el.getBoundingClientRect();
    if (r.width > 0 || r.height > 0) return true;
    // Visually hidden checkboxes/radios/file inputs behind a custom label.
    if (tag === 'input' && /^(checkbox|radio|file)$/.test(inputType(el))) return true;
    var st = style(el);
    return !!st && st.display === 'contents';
  }

  function q(s) { return '"' + s.replace(/"/g, "'") + '"'; }

  // ---- snapshot ------------------------------------------------------------
  function snapshot(opts) {
    opts = opts || {};
    if (opts.mode === 'text') {
      var t = '';
      try { t = document.body ? document.body.innerText || '' : ''; } catch (e) {}
      if (t.length > 50000) t = t.slice(0, 50000) + '\n…';
      return { text: t, count: 0 };
    }
    staleRefs();
    var max = opts.max || 600, widgets = 0, cutShort = false, active = deepActive();
    var out = [];

    function pad(d) { return new Array(d + 1).join('  '); }

    function widgetLine(el, role, tag, depth) {
      var nm = cut(nameOf(el, role, tag), 80);
      var s = pad(depth) + '- ' + role + (nm ? ' ' + q(nm) : '') + ' [ref=' + refOf(el, role) + ']';
      statesOf(el, role, tag, active).forEach(function (st) { s += ' [' + st + ']'; });
      var v = txt(valueOf(el, role, tag));
      if (v && v !== nm) s += ': ' + cut(v, 80);
      if (tag === 'select') {
        var labels = Array.prototype.map.call(el.options, function (o) { return cut(o.label || o.text, 30); });
        if (labels.length) s += ' (options: ' + labels.slice(0, 12).join(' | ') + (labels.length > 12 ? ' | … ' + (labels.length - 12) + ' more' : '') + ')';
      }
      widgets++;
      return s;
    }

    function kids(el, depth, sink, pc) {
      if (el.shadowRoot) {
        var sk = el.shadowRoot.children;
        for (var i = 0; i < sk.length; i++) visit(sk[i], depth, sink, pc);
      }
      var k = el.children;
      for (var j = 0; k && j < k.length; j++) visit(k[j], depth, sink, pc);
    }

    function visit(el, depth, sink, pc) {
      if (widgets >= max) { cutShort = true; return; }
      var tag = tagOf(el);
      if (!tag || /^(script|style|noscript|template|head|meta|link|title|br|wbr)$/.test(tag)) return;
      if (tag === 'svg' && !el.hasAttribute('role')) return;
      if (hidden(el)) return;
      var role = roleOf(el, tag), cursor = pc;
      if (role === 'iframe') return frame(el, depth, sink);
      if (!role) {
        var st = style(el);
        cursor = st ? st.cursor : pc;
        if (cursor === 'pointer' && pc !== 'pointer' && tag !== 'label' && tag !== 'option'
          || el.hasAttribute('onclick') || (el.hasAttribute('tabindex') && el.tabIndex >= 0)) role = 'clickable';
      }
      if (WIDGET[role]) {
        if (!sized(el, tag)) return;
        sink.push(widgetLine(el, role, tag, depth));
        if (!LEAF[role] && tag !== 'select') kids(el, depth + 1, sink, 'pointer');
        return;
      }
      if (role === 'clickable') {
        // A pointer area: listed only when nothing inside it is a widget.
        var inner = [];
        kids(el, depth, inner, 'pointer');
        var hasWidget = inner.some(function (l) { return / \[ref=e\d+\]/.test(l); });
        var nm = cut(nameOf(el, role, tag), 80);
        if (!hasWidget && nm && sized(el, tag)) sink.push(widgetLine(el, 'clickable', tag, depth));
        else Array.prototype.push.apply(sink, inner);
        return;
      }
      if (LANDMARK[role]) {
        var sub = [];
        kids(el, depth + 1, sub, cursor);
        if (sub.length || role === 'dialog' || role === 'alertdialog') {
          var ln = cut(nameOf(el, role, tag), 80);
          sink.push(pad(depth) + '- ' + role + (ln ? ' ' + q(ln) : '') + ':');
          Array.prototype.push.apply(sink, sub);
        }
        return;
      }
      if (role === 'heading') {
        var hn = cut(nameOf(el, role, tag), 100);
        if (hn) sink.push(pad(depth) + '- heading ' + q(hn) + statesOf(el, role, tag, null).map(function (s) { return ' [' + s + ']'; }).join(''));
        kids(el, depth + 1, sink, cursor);
        return;
      }
      if (role === 'alert' || role === 'status') {
        var at = cut(el.innerText, 160);
        if (at) sink.push(pad(depth) + '- ' + role + ' ' + q(at));
        return;
      }
      kids(el, depth, sink, cursor);
    }

    function frame(el, depth, sink) {
      var doc = null, nm = cut(el.getAttribute('title') || el.getAttribute('name') || el.getAttribute('aria-label') || '', 60);
      try { doc = el.contentDocument; } catch (e) {}
      if (!doc || !doc.body) {
        var host = '';
        try { host = new URL(el.src, location.href).host; } catch (e) {}
        if (sized(el, 'iframe')) sink.push(pad(depth) + '- iframe' + (nm ? ' ' + q(nm) : '') + ' (cross-origin' + (host ? ' ' + host : '') + ', not readable)');
        return;
      }
      var sub = [];
      kids(doc.body, depth + 1, sub, 'auto');
      if (sub.length) {
        sink.push(pad(depth) + '- iframe' + (nm ? ' ' + q(nm) : '') + ':');
        Array.prototype.push.apply(sink, sub);
      }
    }

    var root = document.body || document.documentElement;
    if (root) kids(root, 0, out, 'auto');
    if (cutShort) out.push('- … cut at ' + max + ' elements; scroll, or use mode "text" for the page text');
    return { text: out.join('\n'), count: widgets, cut: cutShort };
  }

  // ---- targets -------------------------------------------------------------
  function find(sel) {
    var el = document.querySelector(sel);
    if (!el) throw new Error('no element matches ' + sel);
    return el;
  }

  function target(p, need) {
    if (p.ref) return deref(p.ref);
    if (p.selector) return find(p.selector);
    if (need) throw new Error('pass ref (from browser_snapshot) or selector');
    return null;
  }

  // Password, PIN and one-time-code fields: the outcome says so, so Keel's
  // trace row never shows what was typed.
  function secretField(el) {
    if ((el.type || '').toLowerCase() === 'password') return true;
    var hint = [el.autocomplete, el.name, el.id].join(' ').toLowerCase();
    return /one-time-code|cc-csc|pass|pwd|otp|\bpin\b|cvv|cvc/.test(hint);
  }

  // How an element reads in an outcome: `button "Send" (e12)`.
  function label(el) {
    var tag = tagOf(el), role = roleOf(el, tag) || tag || 'element';
    var nm = cut(nameOf(el, role, tag), 60);
    var r = R.byNode.get(el);
    return role + (nm ? ' ' + q(nm) : '') + (r ? ' (' + r + ')' : '');
  }

  function center(el) {
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    var r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }

  function covered(el, x, y) {
    var doc = el.ownerDocument, top = doc.elementFromPoint(x, y);
    for (var i = 0; top && top.shadowRoot && i < 10; i++) {
      var inner = top.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === top) break;
      top = inner;
    }
    if (!top || top === el || el.contains(top) || top.contains(el)) return null;
    var host = el.getRootNode && el.getRootNode().host;
    if (host && (top === host || host.contains(top))) return null;
    return top;
  }

  function mouse(el, x, y, type, buttons) {
    var w = win(el);
    var init = { bubbles: true, cancelable: true, composed: true, view: w, clientX: x, clientY: y, button: 0, buttons: buttons };
    if (type.indexOf('pointer') === 0) {
      var P = w.PointerEvent || w.MouseEvent;
      return el.dispatchEvent(new P(type, Object.assign({ pointerId: 1, isPrimary: true, pointerType: 'mouse' }, init)));
    }
    return el.dispatchEvent(new w.MouseEvent(type, init));
  }

  function clickEl(el, x, y) {
    mouse(el, x, y, 'pointerdown', 1);
    mouse(el, x, y, 'mousedown', 1);
    if (el.focus) el.focus({ preventScroll: true });
    mouse(el, x, y, 'pointerup', 0);
    mouse(el, x, y, 'mouseup', 0);
    if (el.click) el.click(); else mouse(el, x, y, 'click', 0);
  }

  function setValue(el, text) {
    var w = win(el), tag = tagOf(el);
    var proto = tag === 'textarea' ? w.HTMLTextAreaElement.prototype : tag === 'select' ? w.HTMLSelectElement.prototype : w.HTMLInputElement.prototype;
    var desc = Object.getOwnPropertyDescriptor(proto, 'value');
    if (desc && desc.set) desc.set.call(el, text); else el.value = text;
  }

  function insertText(el, text, replace) {
    var w = win(el);
    if (el.isContentEditable) {
      var doc = el.ownerDocument, sel = w.getSelection();
      if (replace) {
        var range = doc.createRange();
        range.selectNodeContents(el);
        sel.removeAllRanges(); sel.addRange(range);
      }
      if (!doc.execCommand('insertText', false, text)) {
        if (replace) el.textContent = text; else el.textContent += text;
        el.dispatchEvent(new w.InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
      }
      return true;
    }
    if ('value' in el && tagOf(el) !== 'select' && tagOf(el) !== 'button') {
      setValue(el, replace ? text : String(el.value || '') + text);
      el.dispatchEvent(new w.InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
      el.dispatchEvent(new w.Event('change', { bubbles: true }));
      return true;
    }
    return false;
  }

  var KEYS = {
    enter: ['Enter', 'Enter', 13], escape: ['Escape', 'Escape', 27], esc: ['Escape', 'Escape', 27], tab: ['Tab', 'Tab', 9],
    backspace: ['Backspace', 'Backspace', 8], 'delete': ['Delete', 'Delete', 46], space: [' ', 'Space', 32],
    arrowup: ['ArrowUp', 'ArrowUp', 38], arrowdown: ['ArrowDown', 'ArrowDown', 40], arrowleft: ['ArrowLeft', 'ArrowLeft', 37],
    arrowright: ['ArrowRight', 'ArrowRight', 39], up: ['ArrowUp', 'ArrowUp', 38], down: ['ArrowDown', 'ArrowDown', 40],
    left: ['ArrowLeft', 'ArrowLeft', 37], right: ['ArrowRight', 'ArrowRight', 39], home: ['Home', 'Home', 36], end: ['End', 'End', 35],
    pageup: ['PageUp', 'PageUp', 33], pagedown: ['PageDown', 'PageDown', 34]
  };

  function tabbables(doc) {
    var all = doc.querySelectorAll('a[href],button,input,select,textarea,summary,[tabindex],[contenteditable=""],[contenteditable=true]');
    return Array.prototype.filter.call(all, function (el) {
      return !el.disabled && el.tabIndex >= 0 && !hidden(el) && sized(el, tagOf(el)) && inputType(el) !== 'hidden';
    });
  }

  function press(p) {
    var el = target(p, false) || deepActive() || document.body;
    if (p.ref || p.selector) { el.scrollIntoView({ block: 'center', behavior: 'instant' }); if (el.focus) el.focus(); }
    var parts = String(p.key || '').split('+').map(function (s) { return s.trim(); }).filter(Boolean);
    if (!parts.length) throw new Error('key is required, like Enter, Escape, Tab, ArrowDown, or Meta+a');
    var mods = { ctrlKey: false, metaKey: false, altKey: false, shiftKey: false };
    var main = parts.pop();
    parts.forEach(function (m) {
      m = m.toLowerCase();
      if (m === 'control' || m === 'ctrl') mods.ctrlKey = true;
      else if (m === 'meta' || m === 'cmd' || m === 'command') mods.metaKey = true;
      else if (m === 'alt' || m === 'option') mods.altKey = true;
      else if (m === 'shift') mods.shiftKey = true;
      else throw new Error('unknown modifier ' + m);
    });
    var known = KEYS[main.toLowerCase()];
    var key = known ? known[0] : main, code = known ? known[1] : (main.length === 1 ? (/[a-z]/i.test(main) ? 'Key' + main.toUpperCase() : /[0-9]/.test(main) ? 'Digit' + main : '') : main);
    var keyCode = known ? known[2] : (main.length === 1 ? main.toUpperCase().charCodeAt(0) : 0);
    if (!known && main.length !== 1) throw new Error('unknown key ' + main + '; use a single character or Enter, Escape, Tab, Backspace, Delete, Space, Arrow keys, Home, End, PageUp, PageDown');
    var w = win(el);
    var init = Object.assign({ key: key, code: code, keyCode: keyCode, which: keyCode, bubbles: true, cancelable: true, composed: true }, mods);
    var go = el.dispatchEvent(new w.KeyboardEvent('keydown', init));
    var printable = key.length === 1 && !mods.ctrlKey && !mods.metaKey;
    if (go && (printable || key === 'Enter')) el.dispatchEvent(new w.KeyboardEvent('keypress', init));
    var did = '';
    if (go) {
      var tag = tagOf(el), role = roleOf(el, tag);
      if (key === 'Enter') {
        if (tag === 'input' && el.form) {
          if (el.form.requestSubmit) el.form.requestSubmit(); else el.form.submit();
          did = 'submitted the form';
        } else if (role === 'link' || role === 'button') { el.click(); did = 'activated it'; }
      } else if (key === ' ' && (role === 'button' || role === 'checkbox' || role === 'radio' || role === 'switch')) {
        el.click(); did = 'activated it';
      } else if (key === 'Tab') {
        var list = tabbables(el.ownerDocument), at = list.indexOf(el);
        var next = list[(at + (mods.shiftKey ? -1 : 1) + list.length) % list.length];
        if (next) { next.focus(); did = 'focus moved to ' + label(next); }
      } else if ((mods.metaKey || mods.ctrlKey) && key.toLowerCase() === 'a' && el.select) {
        el.select(); did = 'selected all';
      } else if (printable) {
        if (insertText(el, key, false)) did = 'typed it';
      } else if (key === 'Backspace' && 'value' in el && el.value) {
        setValue(el, String(el.value).slice(0, -1));
        el.dispatchEvent(new w.InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' }));
        did = 'deleted a character';
      }
    }
    el.dispatchEvent(new w.KeyboardEvent('keyup', init));
    return 'pressed ' + p.key + ' on ' + label(el) + (did ? ', ' + did : go ? '' : ' (the page handled it)');
  }

  function selectOption(p) {
    var el = target(p, true), tag = tagOf(el);
    var want = (Array.isArray(p.values) ? p.values : [p.values]).map(function (v) { return txt(v).toLowerCase(); }).filter(Boolean);
    if (!want.length) throw new Error('values is required: option labels or values');
    if (tag !== 'select')
      throw new Error(label(el) + ' is not a native <select>; click it to open the list, then click the option');
    if (el.disabled) throw new Error(label(el) + ' is disabled');
    var picked = [], missing = want.slice();
    Array.prototype.forEach.call(el.options, function (o) {
      var hit = [txt(o.value).toLowerCase(), txt(o.label || o.text).toLowerCase()].some(function (s) {
        var i = missing.indexOf(s);
        if (i >= 0) { missing.splice(i, 1); return true; }
        return false;
      });
      if (hit && (el.multiple || !picked.length)) { picked.push(o); }
    });
    if (!picked.length) {
      var have = Array.prototype.map.call(el.options, function (o) { return txt(o.label || o.text); }).slice(0, 20).join(' | ');
      throw new Error('no option matches ' + JSON.stringify(p.values) + ' in ' + label(el) + '; options: ' + have);
    }
    var w = win(el);
    Array.prototype.forEach.call(el.options, function (o) { o.selected = picked.indexOf(o) >= 0; });
    el.dispatchEvent(new w.Event('input', { bubbles: true }));
    el.dispatchEvent(new w.Event('change', { bubbles: true }));
    return 'selected ' + picked.map(function (o) { return q(txt(o.label || o.text)); }).join(', ') + ' in ' + label(el)
      + (missing.length ? ' (no match for ' + missing.join(', ') + ')' : '');
  }

  // ---- actions -------------------------------------------------------------
  function act(op, p) {
    p = p || {};
    if (op === 'click') {
      var el, x, y, note = '';
      if (p.ref || p.selector) {
        el = target(p, true);
        if (el.disabled || el.getAttribute('aria-disabled') === 'true') throw new Error(label(el) + ' is disabled');
        var c = center(el);
        x = c.x; y = c.y;
        var over = covered(el, x, y);
        if (over) note = ' (it was covered by ' + label(over) + '; clicked it directly)';
      } else {
        x = Number(p.x); y = Number(p.y);
        el = document.elementFromPoint(x, y);
        if (!el) throw new Error('nothing at ' + x + ',' + y);
      }
      clickEl(el, x, y);
      if (p.double) { clickEl(el, x, y); mouse(el, x, y, 'dblclick', 0); }
      return (p.double ? 'double-clicked ' : 'clicked ') + label(el) + note;
    }
    if (op === 'type') {
      var t = target(p, false) || deepActive();
      if (!t || t === document.body || t === document.documentElement) throw new Error('no focused element; pass ref or selector');
      if (p.ref || p.selector) { t.scrollIntoView({ block: 'center', behavior: 'instant' }); if (t.focus) t.focus(); }
      if (t.disabled || t.readOnly) throw new Error(label(t) + ' is ' + (t.disabled ? 'disabled' : 'read-only'));
      var text = String(p.text == null ? '' : p.text);
      if (!insertText(t, text, true)) throw new Error(label(t) + ' does not take text');
      var submitted = '';
      if (p.submit) submitted = press({ key: 'Enter' }).indexOf('submitted the form') >= 0 ? ', submitted the form' : ', pressed Enter';
      return 'typed ' + text.length + ' chars into ' + label(t) + (secretField(t) ? ' (password)' : '') + submitted;
    }
    if (op === 'select') return selectOption(p);
    if (op === 'press') return press(p);
    if (op === 'upload_target') {
      var u = target(p, true), ut = tagOf(u);
      var isFile = ut === 'input' && inputType(u) === 'file';
      if (isFile && u.disabled) throw new Error(label(u) + ' is disabled');
      var cu = center(u);
      clickEl(u, cu.x, cu.y);
      return JSON.stringify({ file_input: isFile, multiple: isFile && u.multiple, label: label(u) });
    }
    if (op === 'scroll') return scroll(p);
    throw new Error('unknown op ' + op);
  }

  function scroll(p) {
    var dx = Number(p.dx) || 0, dy = Number(p.dy) || 0, to = String(p.to || '');
    var root = document.scrollingElement || document.documentElement;
    function scrollable(el) {
      if (el === root || el === document.body) return false;
      var st = style(el);
      return !!st && (el.scrollHeight > el.clientHeight + 1 || el.scrollWidth > el.clientWidth + 1)
        && /(auto|scroll|overlay)/.test(st.overflow + ' ' + st.overflowX + ' ' + st.overflowY);
    }
    function pageScrolls() { return root.scrollHeight > innerHeight + 1 || root.scrollWidth > innerWidth + 1; }
    function biggest() {
      var best = null, area = 0, all = document.querySelectorAll('body *');
      for (var i = 0; i < all.length && i < 8000; i++) {
        var el = all[i];
        if (!scrollable(el)) continue;
        var r = el.getBoundingClientRect(), a = Math.max(0, r.width) * Math.max(0, r.height);
        if (a > area) { area = a; best = el; }
      }
      return best;
    }
    var tgt = null, what = 'page';
    if (p.ref || p.selector) {
      var sel = target(p, true);
      if (scrollable(sel)) { tgt = sel; what = label(sel); }
      else {
        sel.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' });
        what = 'page (' + label(sel) + ' in view)';
        dx = 0; dy = 0; to = '';
      }
    } else if (!pageScrolls()) {
      var big = biggest();
      if (big) { tgt = big; what = label(big); }
    }
    var el = tgt || root, left = el.scrollLeft, top = el.scrollTop;
    if (to === 'top') top = 0;
    else if (to === 'bottom') top = el.scrollHeight;
    else if (to === 'left') left = 0;
    else if (to === 'right') left = el.scrollWidth;
    else { left += dx; top += dy; }
    if (dx || dy || to) {
      if (tgt) tgt.scrollTo({ left: left, top: top, behavior: 'instant' });
      else window.scrollTo({ left: left, top: top, behavior: 'instant' });
    }
    var pos = {
      x: Math.round(tgt ? el.scrollLeft : scrollX), y: Math.round(tgt ? el.scrollTop : scrollY),
      width: el.scrollWidth, height: el.scrollHeight,
      viewport_w: tgt ? el.clientWidth : innerWidth, viewport_h: tgt ? el.clientHeight : innerHeight,
      target: what
    };
    lastScroll = pos;
    return 'scrolled ' + what + ' to ' + pos.x + ',' + pos.y + ' (content ' + pos.width + 'x' + pos.height + ', viewport ' + pos.viewport_w + 'x' + pos.viewport_h + ')';
  }
  var lastScroll = null;

  // ---- waits ---------------------------------------------------------------
  function deepText(root, sink, budget) {
    if (!root) return;
    try { sink.push(root.innerText || ''); } catch (e) {}
    var all = root.querySelectorAll ? root.querySelectorAll('*') : [];
    for (var i = 0; i < all.length && i < budget; i++) {
      var el = all[i];
      if (el.shadowRoot) {
        var parts = [];
        Array.prototype.forEach.call(el.shadowRoot.children, function (c) { parts.push(c.innerText || c.textContent || ''); });
        sink.push(parts.join(' '));
      }
      if (tagOf(el) === 'iframe') { try { if (el.contentDocument) deepText(el.contentDocument.body, sink, 2000); } catch (e) {} }
    }
  }

  function visible(el) { return el.isConnected && !hidden(el) && sized(el, tagOf(el)); }

  // One check of a wait condition: {ok, detail}.
  function check(p) {
    var gone = !!p.gone;
    if (p.text) {
      var sink = [];
      deepText(document.body, sink, 20000);
      var hay = txt(sink.join(' ')).toLowerCase(), needle = txt(p.text).toLowerCase();
      var has = hay.indexOf(needle) >= 0;
      return { ok: gone ? !has : has, detail: 'text ' + JSON.stringify(p.text) + (has ? ' is on the page' : ' is not on the page') };
    }
    if (p.ref || p.selector) {
      var el = null, why = '';
      try { el = target(p, true); } catch (e) { why = String(e.message || e); }
      var vis = !!el && visible(el);
      return { ok: gone ? !vis : vis, detail: (p.ref || p.selector) + (vis ? ' is visible' : el ? ' is not visible' : ' is not on the page' + (why && !gone ? ' (' + why + ')' : '')) };
    }
    return { ok: true, detail: '' };
  }

  // ---- upload fallback -----------------------------------------------------
  function upload(p) {
    var el = target(p, true);
    if (tagOf(el) !== 'input' || inputType(el) !== 'file') throw new Error(label(el) + ' is not a file input');
    var w = win(el), dt = new w.DataTransfer();
    (p.files || []).forEach(function (f) {
      var bin = atob(f.data), bytes = new Uint8Array(bin.length);
      for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      dt.items.add(new w.File([bytes], f.name, { type: f.type || 'application/octet-stream' }));
    });
    el.files = dt.files;
    el.dispatchEvent(new w.Event('input', { bubbles: true }));
    el.dispatchEvent(new w.Event('change', { bubbles: true }));
    return 'set ' + dt.files.length + ' file(s) on ' + label(el);
  }

  function wrap(fn) {
    return function () {
      var o;
      try { o = fn.apply(null, arguments); if (typeof o === 'string') o = { ok: true, text: o }; else o.ok = o.ok !== false; }
      catch (e) { o = { ok: false, error: String((e && e.message) || e) }; }
      o.url = location.href || '';
      o.title = document.title || '';
      if (lastScroll) { o.scroll = lastScroll; lastScroll = null; }
      return JSON.stringify(o);
    };
  }

  var api = {
    snapshot: wrap(snapshot),
    act: wrap(function (op, p) { var t = act(op, p); return op === 'upload_target' ? { ok: true, text: t } : t; }),
    check: wrap(check),
    upload: wrap(upload),
    // Tests: ref bookkeeping.
    refCount: function () { return R.byRef.size; }
  };
  Object.defineProperty(window, '__keelAgent', { value: api });
  return api;
})()
