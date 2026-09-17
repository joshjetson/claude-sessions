
(function () {
  var DATA = JSON.parse(document.getElementById('data').textContent);
  var byId = {};
  DATA.entries.forEach(function (e) { byId[e.id] = e; });
  var tasksById = {};
  DATA.tasks.forEach(function (t) { tasksById[t.taskId] = t; });

  var state = { tab: 'journal', q: '', repo: '', range: '', linked: false, rev: false, sel: null };

  var $ = function (id) { return document.getElementById(id); };
  var esc = function (s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  };
  var bytes = function (n) {
    if (!n) return '0 B';
    var u = ['B', 'KB', 'MB', 'GB'], i = 0;
    while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
    return (n >= 10 || i === 0 ? Math.round(n) : n.toFixed(1)) + ' ' + u[i];
  };
  var fileUrl = function (p) { return 'file://' + String(p).split('/').map(encodeURIComponent).join('/'); };
  // List rows show plain text — strip the inline markdown the source carries.
  var plain = function (s) {
    return String(s == null ? '' : s)
      .replace(/`([^`]*)`/g, '$1')
      .replace(/\*\*([^*]*)\*\*/g, '$1')
      .replace(/(^|[^*])\*([^*\n]+)\*/g, '$1$2')
      .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
      .replace(/\s+/g, ' ')
      .trim();
  };

  // --- mini markdown ---
  function md(src) {
    var out = [], lines = String(src || '').split('\n');
    var i = 0, para = [], list = null, quote = [];
    function flushPara() { if (para.length) { out.push('<p>' + inline(para.join(' ')) + '</p>'); para = []; } }
    function flushList() { if (list) { out.push('<ul>' + list.join('') + '</ul>'); list = null; } }
    function flushQuote() { if (quote.length) { out.push('<blockquote><p>' + inline(quote.join(' ')) + '</p></blockquote>'); quote = []; } }
    function flushAll() { flushPara(); flushList(); flushQuote(); }
    while (i < lines.length) {
      var line = lines[i];
      if (/^\s*```/.test(line)) {
        flushAll();
        var buf = [];
        i++;
        while (i < lines.length && !/^\s*```/.test(lines[i])) { buf.push(lines[i]); i++; }
        i++;
        out.push('<pre><code>' + esc(buf.join('\n')) + '</code></pre>');
        continue;
      }
      if (!line.trim()) { flushAll(); i++; continue; }
      var h = line.match(/^#{1,6}\s+(.*)$/);
      if (h) { flushAll(); out.push('<h4>' + inline(h[1]) + '</h4>'); i++; continue; }
      var q = line.match(/^\s*>\s?(.*)$/);
      if (q) { flushPara(); flushList(); quote.push(q[1]); i++; continue; }
      var li = line.match(/^\s*(?:[-*]|\d+\.)\s+(.*)$/);
      if (li) { flushPara(); flushQuote(); if (!list) list = []; list.push('<li>' + inline(li[1]) + '</li>'); i++; continue; }
      flushList(); flushQuote();
      para.push(line.trim());
      i++;
    }
    flushAll();
    return out.join('');
  }
  function inline(s) {
    return esc(s)
      .replace(/`([^`]+)`/g, function (m, c) { return '<code>' + c + '</code>'; })
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(/(^|[^*])\*([^*\n]+)\*/g, '$1<em>$2</em>')
      .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>');
  }

  function highlight(text, q) {
    var t = esc(text);
    if (!q) return t;
    try {
      return t.replace(new RegExp('(' + q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + ')', 'ig'), '<mark>$1</mark>');
    } catch (e) { return t; }
  }

  function withinRange(dateStr) {
    if (!state.range || !dateStr) return true;
    var cutoff = new Date(DATA.generatedAt).getTime() - Number(state.range) * 86400000;
    return new Date(dateStr + 'T00:00:00').getTime() >= cutoff;
  }

  function hasArchive(e) {
    return e.taskIds.some(function (id) { return tasksById[id]; });
  }

  // --- filtering ---
  function entries() {
    var q = state.q.toLowerCase();
    return DATA.entries.filter(function (e) {
      if (state.repo && e.repo !== state.repo) return false;
      if (state.rev && !e.isRevision) return false;
      if (state.linked && !hasArchive(e)) return false;
      if (!withinRange(e.date)) return false;
      if (!q) return true;
      return (e.title + ' ' + e.body + ' ' + e.taskIds.join(' ')).toLowerCase().indexOf(q) !== -1;
    });
  }
  function rules() {
    var q = state.q.toLowerCase();
    return DATA.rules.filter(function (r) {
      if (state.repo && r.repo !== state.repo) return false;
      if (!q) return true;
      return r.text.toLowerCase().indexOf(q) !== -1;
    });
  }
  function tasks() {
    var q = state.q.toLowerCase();
    return DATA.tasks.filter(function (t) {
      if (state.repo && t.repo !== state.repo) return false;
      if (state.linked && !t.entryIds.length) return false;
      if (!q) return true;
      var hay = t.taskId + ' ' + t.repo + ' ' + t.logs.map(function (l) { return l.title + ' ' + l.summary; }).join(' ');
      return hay.toLowerCase().indexOf(q) !== -1;
    });
  }

  // --- list rendering ---
  function render() {
    var list = $('list'), rows = [], n = 0;
    if (state.tab === 'journal') {
      var es = entries(); n = es.length;
      rows = es.map(function (e) {
        var f = [];
        if (e.isRevision) f.push('<span class="flag rev">revision</span>');
        if (e.hasAssumption) f.push('<span class="flag assume">wrong assumption</span>');
        if (e.hasCorrection) f.push('<span class="flag correct">user correction</span>');
        if (e.hasAttempts) f.push('<span class="flag">failed attempts</span>');
        if (e.hasRule) f.push('<span class="flag rule">rule</span>');
        if (hasArchive(e)) f.push('<span class="flag arch">archive</span>');
        var tasksLabel = e.taskIds.length ? e.taskIds.map(function (t) { return '#' + t; }).join(' ') : '';
        return '<div class="row' + (state.sel === e.id ? ' sel' : '') + '" data-id="' + esc(e.id) + '">' +
          '<div class="meta"><span>' + esc(e.date || '—') + '</span><span class="repo-tag">' + esc(e.repo) + '</span><span>' + esc(tasksLabel) + '</span></div>' +
          '<div class="title">' + highlight(plain(e.title), state.q) + '</div>' +
          '<div class="flags">' + f.join('') + '</div>' +
        '</div>';
      });
    } else if (state.tab === 'rules') {
      var rs = rules(); n = rs.length;
      rows = rs.map(function (r) {
        return '<div class="row' + (state.sel === r.id ? ' sel' : '') + '" data-id="' + esc(r.id) + '">' +
          '<div class="meta"><span class="repo-tag">' + esc(r.repo) + '</span><span>' + esc(r.section || '') + '</span></div>' +
          '<div class="sub">' + highlight(plain(r.text).slice(0, 260) + (r.text.length > 260 ? '…' : ''), state.q) + '</div>' +
        '</div>';
      });
    } else if (state.tab === 'tasks') {
      var ts = tasks(); n = ts.length;
      rows = ts.map(function (t) {
        var log = t.logs[t.logs.length - 1];
        var f = [];
        if (t.entryIds.length) f.push('<span class="flag rule">journal</span>');
        else f.push('<span class="flag">no journal entry</span>');
        if (t.transcript) f.push('<span class="flag arch">' + bytes(t.transcript.bytes) + '</span>');
        if (t.prompts.length) f.push('<span class="flag">' + t.prompts.length + ' prompt' + (t.prompts.length > 1 ? 's' : '') + '</span>');
        return '<div class="row' + (state.sel === 'task:' + t.taskId ? ' sel' : '') + '" data-id="task:' + t.taskId + '">' +
          '<div class="meta"><span>#' + t.taskId + '</span><span class="repo-tag">' + esc(t.repo || '—') + '</span><span>' + esc((t.archivedAt || '').slice(0, 10)) + '</span></div>' +
          '<div class="title">' + highlight(log ? log.title : '(no daily-log line)', state.q) + '</div>' +
          '<div class="flags">' + f.join('') + '</div>' +
        '</div>';
      });
    } else {
      n = DATA.repos.length;
      rows = DATA.repos.map(function (r) {
        return '<div class="row" data-id="repo:' + esc(r.name) + '">' +
          '<div class="meta"><span class="repo-tag">' + esc(r.name) + '</span><span>' + esc(r.lastEntry || '—') + '</span></div>' +
          '<div class="sub">' + r.entries + ' entries · ' + r.rules + ' rules</div>' +
          '<div class="flags"><span class="bar" style="width:' + Math.max(4, r.entries * 6) + 'px"></span></div>' +
        '</div>';
      });
    }
    list.innerHTML = rows.join('') || '<div class="empty">Nothing matches.</div>';
    $('count').textContent = n + ' ' + (state.tab === 'tasks' ? 'tasks' : state.tab === 'rules' ? 'rules' : state.tab === 'repos' ? 'repos' : 'entries');
    Array.prototype.forEach.call(list.querySelectorAll('.row'), function (el) {
      el.addEventListener('click', function () { select(el.getAttribute('data-id')); });
    });
  }

  function archiveBox(taskId) {
    var t = tasksById[taskId];
    if (!t) return '<div class="box"><h3>Task #' + taskId + '</h3><div class="empty">No archived session for this task.</div></div>';
    var kv = [];
    if (t.cwd) kv.push('<dt>cwd</dt><dd>' + esc(t.cwd) + '</dd>');
    if (t.sessionId) kv.push('<dt>session</dt><dd>' + esc(t.sessionId) + '</dd>');
    if (t.archivedAt) kv.push('<dt>archived</dt><dd>' + esc(t.archivedAt.replace('T', ' ').slice(0, 16)) + '</dd>');
    if (t.transcript) kv.push('<dt>transcript</dt><dd><a href="' + fileUrl(t.transcript.path) + '">' + esc(t.transcript.path) + '</a> (' + bytes(t.transcript.bytes) + ')</dd>');
    if (t.prompts.length) {
      kv.push('<dt>prompts</dt><dd>' + t.prompts.map(function (p) {
        return '<a href="' + fileUrl(p.path) + '">' + esc(p.at.replace('T', ' ').slice(0, 16)) + '</a>';
      }).join('<br>') + '</dd>');
    }
    if (t.logs.length) {
      kv.push('<dt>daily log</dt><dd>' + t.logs.map(function (l) {
        return esc(l.date) + ' — ' + esc(l.summary || l.title) + (l.mrUrl ? ' <a href="' + esc(l.mrUrl) + '" target="_blank" rel="noreferrer">MR</a>' : '');
      }).join('<br>') + '</dd>');
    }
    var cmd = t.cwd && t.sessionId ? 'cd ' + t.cwd + ' && claude --resume ' + t.sessionId : '';
    return '<div class="box"><h3>Task archive · #' + t.taskId + '</h3>' +
      '<dl class="kv">' + kv.join('') + '</dl>' +
      (cmd ? '<div class="cmd"><code>' + esc(cmd) + '</code><button class="copy" data-copy="' + esc(cmd) + '">copy</button></div>' : '') +
    '</div>';
  }

  function select(id) {
    state.sel = id;
    var d = $('detail');
    if (id && id.indexOf('task:') === 0) {
      var t = tasksById[Number(id.slice(5))];
      var linked = (t.entryIds || []).map(function (eid) { return byId[eid]; }).filter(Boolean);
      d.innerHTML = '<div class="dhead"><div class="dmeta"><span>' + esc(t.repo || '—') + '</span></div>' +
        '<h2>Task #' + t.taskId + (t.logs.length ? ' — ' + esc(t.logs[t.logs.length - 1].title) : '') + '</h2></div>' +
        archiveBox(t.taskId) +
        (linked.length
          ? '<div class="box"><h3>Journal entries</h3>' + linked.map(function (e) {
              return '<div><a class="chip" data-goto="' + esc(e.id) + '">' + esc(e.date || '') + ' ' + esc(e.repo) + '</a> ' + esc(plain(e.title)) + '</div>';
            }).join('') + '</div>'
          : '<div class="box"><h3>Journal entries</h3><div class="empty">No reasoning journal entry references this task.</div></div>');
    } else if (id && id.indexOf('repo:') === 0) {
      var name = id.slice(5);
      var r = DATA.repos.filter(function (x) { return x.name === name; })[0];
      var es = DATA.entries.filter(function (e) { return e.repo === name; });
      d.innerHTML = '<div class="dhead"><h2>' + esc(name) + '</h2><div class="dmeta"><span>' + esc(r.path) + '</span></div></div>' +
        '<div class="box"><h3>Files</h3><dl class="kv">' +
          '<dt>journal</dt><dd><a href="' + fileUrl(r.path + '/docs/agent/problem-reasoning-journal.md') + '">docs/agent/problem-reasoning-journal.md</a></dd>' +
          '<dt>rules</dt><dd><a href="' + fileUrl(r.path + '/docs/agent/project-rules.md') + '">docs/agent/project-rules.md</a></dd>' +
          '<dt>entries</dt><dd>' + r.entries + '</dd><dt>rules</dt><dd>' + r.rules + '</dd>' +
        '</dl></div>' +
        '<div class="box"><h3>Entries</h3>' + es.map(function (e) {
          return '<div><a class="chip" data-goto="' + esc(e.id) + '">' + esc(e.date || '') + '</a> ' + esc(plain(e.title)) + '</div>';
        }).join('') + '</div>';
    } else if (byId[id]) {
      var e = byId[id];
      var chips = e.taskIds.map(function (t) {
        return '<a class="chip" data-task="' + t + '">#' + t + (tasksById[t] ? ' ↗' : '') + '</a>';
      }).join(' ');
      d.innerHTML = '<div class="dhead">' +
          '<div class="dmeta"><span>' + esc(e.date || '—') + '</span><span class="repo-tag">' + esc(e.repo) + '</span>' + chips + '</div>' +
          '<h2>' + esc(plain(e.title)) + '</h2>' +
          '<div class="dmeta"><a class="chip plain" href="' + fileUrl(e.file) + '">' + esc(e.file.replace(/^.*\/docs\//, 'docs/')) + '</a><span>' + e.words + ' words</span></div>' +
        '</div>' +
        e.sections.map(function (s) {
          return '<div class="sec k-' + esc(s.kind) + '">' + (s.label ? '<h3>' + esc(s.label) + '</h3>' : '') +
            '<div class="md">' + md(s.body) + '</div></div>';
        }).join('') +
        e.taskIds.map(archiveBox).join('');
    } else {
      var r2 = DATA.rules.filter(function (x) { return x.id === id; })[0];
      if (!r2) { d.innerHTML = '<div class="empty">Select an entry.</div>'; render(); return; }
      d.innerHTML = '<div class="dhead"><div class="dmeta"><span class="repo-tag">' + esc(r2.repo) + '</span><span>' + esc(r2.section || '') + '</span></div></div>' +
        '<div class="md">' + md(r2.text) + '</div>' +
        (r2.taskIds.length ? '<div class="box"><h3>Referenced tasks</h3>' + r2.taskIds.map(function (t) {
          return '<a class="chip" data-task="' + t + '">#' + t + '</a> ';
        }).join('') + '</div>' : '') +
        '<div class="box"><h3>Source</h3><dl class="kv"><dt>file</dt><dd><a href="' + fileUrl(r2.file) + '">' + esc(r2.file) + '</a></dd></dl></div>';
    }
    wireDetail();
    render();
  }

  function wireDetail() {
    var d = $('detail');
    Array.prototype.forEach.call(d.querySelectorAll('[data-copy]'), function (b) {
      b.addEventListener('click', function () {
        navigator.clipboard.writeText(b.getAttribute('data-copy'));
        b.textContent = 'copied';
        setTimeout(function () { b.textContent = 'copy'; }, 1200);
      });
    });
    Array.prototype.forEach.call(d.querySelectorAll('[data-task]'), function (a) {
      a.addEventListener('click', function () {
        var t = Number(a.getAttribute('data-task'));
        if (!tasksById[t]) return;
        setTab('tasks'); select('task:' + t);
      });
    });
    Array.prototype.forEach.call(d.querySelectorAll('[data-goto]'), function (a) {
      a.addEventListener('click', function () { setTab('journal'); select(a.getAttribute('data-goto')); });
    });
  }

  function setTab(name) {
    state.tab = name;
    state.sel = null;
    Array.prototype.forEach.call(document.querySelectorAll('.tab'), function (t) {
      t.classList.toggle('active', t.getAttribute('data-tab') === name);
    });
    $('detail').innerHTML = '<div class="empty">Select an entry.</div>';
    // Only the journal tab uses every filter; dim the ones that do nothing here.
    var applies = {
      range: name === 'journal',
      onlyRev: name === 'journal',
      onlyLinked: name === 'journal' || name === 'tasks',
      repo: name !== 'repos',
    };
    Object.keys(applies).forEach(function (k) {
      var el = $(k);
      el.disabled = !applies[k];
      var wrap = el.closest('.chk') || el;
      wrap.style.opacity = applies[k] ? '1' : '.35';
    });
    $('onlyLinked').parentNode.lastChild.textContent = name === 'tasks' ? ' has a journal entry' : ' linked to an archive';
    render();
  }

  // --- boot ---
  $('gen').textContent = 'generated ' + DATA.generatedAt.replace('T', ' ').slice(0, 16) +
    ' · ' + DATA.entries.length + ' entries · ' + DATA.rules.length + ' rules · ' + DATA.tasks.length + ' archived tasks';

  var repoSel = $('repo');
  repoSel.innerHTML = '<option value="">All repos</option>' + DATA.repos.map(function (r) {
    return '<option value="' + esc(r.name) + '">' + esc(r.name) + ' (' + r.entries + ')</option>';
  }).join('');

  Array.prototype.forEach.call(document.querySelectorAll('.tab'), function (t) {
    t.addEventListener('click', function () { setTab(t.getAttribute('data-tab')); });
  });
  $('q').addEventListener('input', function (e) { state.q = e.target.value.trim(); render(); });
  repoSel.addEventListener('change', function (e) { state.repo = e.target.value; render(); });
  $('range').addEventListener('change', function (e) { state.range = e.target.value; render(); });
  $('onlyLinked').addEventListener('change', function (e) { state.linked = e.target.checked; render(); });
  $('onlyRev').addEventListener('change', function (e) { state.rev = e.target.checked; render(); });
  document.addEventListener('keydown', function (e) {
    if (e.key === '/' && document.activeElement !== $('q')) { e.preventDefault(); $('q').focus(); }
    if (e.key === 'Escape') { $('q').value = ''; state.q = ''; render(); }
  });

  setTab('journal');
})();
