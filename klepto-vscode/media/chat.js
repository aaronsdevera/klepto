(function () {
  const vscode = acquireVsCodeApi();
  const state = vscode.getState() || {
    sessions: {},
    activeId: null,
    tabOrder: [],
    prefs: { agentMode: 'agent', profile: 'coding', provider: '', model: '' },
  };
  if (!state.prefs) {
    state.prefs = { agentMode: 'agent', profile: 'coding', provider: '', model: '' };
  }

  const els = {
    tabs: document.getElementById('tabs'),
    messages: document.getElementById('messages'),
    empty: document.getElementById('emptyState'),
    input: document.getElementById('composerInput'),
    send: document.getElementById('sendMessage'),
    newSession: document.getElementById('newSession'),
    moreBtn: document.getElementById('moreBtn'),
    moreMenu: document.getElementById('moreMenu'),
    status: document.getElementById('status'),
    modeToggle: document.getElementById('modeToggle'),
    profileSelect: document.getElementById('profileSelect'),
    providerSelect: document.getElementById('providerSelect'),
    modelSelect: document.getElementById('modelSelect'),
    prefsHint: document.getElementById('prefsHint'),
    attachBtn: document.getElementById('attachBtn'),
    attachRow: document.getElementById('attachRow'),
    mentionMenu: document.getElementById('mentionMenu'),
    connLight: document.getElementById('connLight'),
    dropZone: document.getElementById('dropZone'),
    composerBox: document.querySelector('.composer-box'),
  };

  let modelCatalog = [];
  let profileCatalog = {};
  let pendingAttachments = [];
  let mentionState = { open: false, query: '', index: 0, candidates: [], requestId: null };
  let pendingFetches = new Map();
  let pendingRequests = new Map();
  const planContinueTimers = new Map();
  let newSessionPending = false;
  let reqCounter = 0;
  let workspaceRoot = '';

  const FILE_CHANGE_TOOLS = new Set(['write', 'edit']);
  const FILE_PATH_TOOLS = new Set(['read', 'write', 'edit']);

  function nextRequestId(prefix) {
    reqCounter += 1;
    return `${prefix}-${Date.now()}-${reqCounter}`;
  }

  function persist() {
    vscode.setState(state);
  }

  function postPrefs() {
    vscode.postMessage({
      type: 'setPrefs',
      agentMode: state.prefs.agentMode || 'agent',
      profile: state.prefs.profile || profileForMode(state.prefs.agentMode || 'agent'),
      provider: state.prefs.provider || '',
      model: state.prefs.model || '',
    });
    updatePrefsHint();
  }

  function updatePrefsHint() {
    const mode = (state.prefs.agentMode || 'agent').replace(/^./, (c) => c.toUpperCase());
    const model =
      state.prefs.model ||
      (state.prefs.provider ? `${state.prefs.provider}/default` : 'default model');
    const profile = state.prefs.profile || profileForMode(state.prefs.agentMode || 'agent');
    const tip = `${profile} profile · ${mode} · ${model} · Enter send`;
    if (els.prefsHint) {
      els.prefsHint.textContent = tip;
      els.prefsHint.title = tip;
    }
    if (els.input) {
      els.input.title = tip;
    }
    // Keep mode button titles current
    for (const btn of els.modeToggle.querySelectorAll('.mode-btn')) {
      const m = btn.getAttribute('data-mode') || '';
      const base =
        m === 'plan'
          ? 'Plan — read-only planning'
          : m === 'debug'
            ? 'Debug — focused debugging'
            : 'Agent — full tools';
      btn.title = m === (state.prefs.agentMode || 'agent') ? `${base} (active)` : base;
    }
  }

  function setMode(mode, syncProfile = true) {
    state.prefs.agentMode = mode;
    if (syncProfile) state.prefs.profile = profileForMode(mode);
    for (const btn of els.modeToggle.querySelectorAll('.mode-btn')) {
      btn.classList.toggle('active', btn.getAttribute('data-mode') === mode);
    }
    persist();
    postPrefs();
  }

  function profileForMode(mode) {
    return mode === 'agent' ? 'coding' : mode;
  }

  function fillProfiles(profiles) {
    const names = Object.keys(profiles || {});
    const defaults = ['coding', 'plan', 'debug'];
    const all = [...new Set([...defaults, ...names])];
    const current = state.prefs.profile || profileForMode(state.prefs.agentMode || 'agent');
    els.profileSelect.innerHTML = '';
    for (const name of all) {
      const opt = document.createElement('option');
      opt.value = name;
      opt.textContent = name;
      const description = profiles?.[name]?.description;
      if (description) opt.title = description;
      els.profileSelect.appendChild(opt);
    }
    els.profileSelect.value = current;
    els.profileSelect.title = `Profile: ${current}`;
  }

  function fillProviders(providers) {
    const current = state.prefs.provider || '';
    els.providerSelect.innerHTML = '';
    const def = document.createElement('option');
    def.value = '';
    def.textContent = 'Provider';
    els.providerSelect.appendChild(def);
    for (const p of providers) {
      const opt = document.createElement('option');
      opt.value = p;
      opt.textContent = p;
      els.providerSelect.appendChild(opt);
    }
    els.providerSelect.value = [...els.providerSelect.options].some((o) => o.value === current)
      ? current
      : '';
    els.providerSelect.title = els.providerSelect.value
      ? `Provider: ${els.providerSelect.value}`
      : 'Provider (default)';
  }

  function fillModels() {
    const provider = state.prefs.provider || '';
    const current = state.prefs.model || '';
    const filtered = provider
      ? modelCatalog.filter((m) => m.provider === provider)
      : modelCatalog;

    els.modelSelect.innerHTML = '';
    const def = document.createElement('option');
    def.value = '';
    def.textContent = 'Model';
    els.modelSelect.appendChild(def);

    for (const m of filtered) {
      const opt = document.createElement('option');
      opt.value = m.label;
      // Prefer short id in the closed control; full label via title/hover
      opt.textContent = provider ? m.id : (m.id || m.label);
      opt.title = m.label;
      els.modelSelect.appendChild(opt);
    }

    els.modelSelect.value = [...els.modelSelect.options].some((o) => o.value === current)
      ? current
      : '';
    els.modelSelect.title = els.modelSelect.value
      ? `Model: ${els.modelSelect.value}`
      : 'Model (default)';
  }

  function applyModelsUpdate(msg) {
    modelCatalog = msg.models || [];
    const providers =
      msg.providers && msg.providers.length
        ? msg.providers
        : [...new Set(modelCatalog.map((m) => m.provider))];
    if (msg.prefs) {
      if (msg.prefs.agentMode) state.prefs.agentMode = msg.prefs.agentMode;
      if (msg.prefs.profile) state.prefs.profile = msg.prefs.profile;
      if (msg.prefs.provider != null) state.prefs.provider = msg.prefs.provider || '';
      if (msg.prefs.model != null) state.prefs.model = msg.prefs.model || '';
    }
    let unavailable = '';
    if (
      state.prefs.model &&
      !modelCatalog.some((model) => model.label === state.prefs.model)
    ) {
      unavailable = `Model unavailable: ${state.prefs.model}. Select an available model.`;
      state.prefs.model = '';
    }
    if (state.prefs.provider && !providers.includes(state.prefs.provider)) {
      state.prefs.provider = '';
    }
    fillProviders(providers);
    fillModels();
    setMode(state.prefs.agentMode || 'agent', false);
    persist();
    postPrefs();
    if (unavailable) setStatus(unavailable);
    else if (msg.message) setStatus(msg.message);
  }

  function applyProfilesUpdate(msg) {
    profileCatalog = msg.profiles || {};
    if (msg.selected) state.prefs.profile = msg.selected;
    fillProfiles(profileCatalog);
    persist();
    postPrefs();
  }

  function ensureSession(id, meta) {
    if (!state.sessions[id]) {
      state.sessions[id] = {
        id,
        title: meta?.title || `Chat ${id.slice(-6)}`,
        status: meta?.status || 'Waiting',
        messages: [],
        generating: false,
        streamingMsgId: null,
        thinkingBlockId: null,
        stopPending: false,
      };
      if (!state.tabOrder.includes(id)) state.tabOrder.push(id);
    } else if (meta) {
      Object.assign(state.sessions[id], meta);
    }
    const session = state.sessions[id];
    if (typeof session.generating !== 'boolean') session.generating = false;
    if (!('streamingMsgId' in session)) session.streamingMsgId = null;
    if (!('thinkingBlockId' in session)) session.thinkingBlockId = null;
    if (typeof session.stopPending !== 'boolean') session.stopPending = false;
    return session;
  }

  function active() {
    return state.activeId ? state.sessions[state.activeId] : null;
  }

  function setStatus(text) {
    els.status.textContent = text || '';
  }

  function setConnected(connected) {
    if (!els.connLight) return;
    els.connLight.classList.remove('is-unknown', 'is-on', 'is-off');
    if (connected) {
      els.connLight.classList.add('is-on');
      els.connLight.title = 'Daemon connected';
      els.connLight.setAttribute('aria-label', 'Daemon connected');
    } else {
      els.connLight.classList.add('is-off');
      els.connLight.title = 'Daemon disconnected';
      els.connLight.setAttribute('aria-label', 'Daemon disconnected');
    }
  }

  function setGenerating(on) {
    const session = active();
    if (session) session.generating = on;
    els.send.classList.toggle('is-stop', !!on);
    els.send.classList.toggle('is-stopping', !!session?.stopPending);
    els.send.title = session?.stopPending ? 'Stopping…' : on ? 'Stop (Esc)' : 'Send (Enter)';
    els.send.setAttribute('aria-label', on ? 'Stop' : 'Send');
    els.send.disabled = !!session?.stopPending;
    vscode.postMessage({ type: 'generationState', generating: !!on });
  }

  function isGenerating() {
    return !!active()?.generating;
  }

  function syncGeneratingUi() {
    setGenerating(!!active()?.generating);
  }

  function renderTabs() {
    els.tabs.innerHTML = '';
    for (const id of state.tabOrder) {
      const s = state.sessions[id];
      if (!s) continue;
      const tab = document.createElement('button');
      tab.className = 'tab' + (id === state.activeId ? ' active' : '');
      tab.title = s.title;
      tab.innerHTML =
        `<span class="tab-label"></span><span class="tab-close" title="Close">×</span>`;
      tab.querySelector('.tab-label').textContent = s.title;
      tab.addEventListener('click', (e) => {
        if (e.target.classList.contains('tab-close')) {
          closeSession(id);
          return;
        }
        switchSession(id);
      });
      els.tabs.appendChild(tab);
    }
  }

  const renderMarkdown = window.KleptoMarkdown?.render || ((text) => escapeHtml(text));

  function wireMarkdownLinks(container) {
    container.querySelectorAll('[data-md-link]').forEach((link) => {
      link.addEventListener('click', (event) => {
        event.preventDefault();
        const target = decodeURIComponent(link.dataset.mdLink || '');
        if (target) vscode.postMessage({ type: 'openMarkdownLink', target });
      });
    });
  }

  function parseToolArgs(args) {
    if (!args) return {};
    if (typeof args === 'object') return args;
    try {
      const parsed = JSON.parse(String(args));
      return parsed && typeof parsed === 'object' ? parsed : {};
    } catch {
      return {};
    }
  }

  function toolFilePath(name, args) {
    const n = String(name || '').toLowerCase();
    if (!FILE_PATH_TOOLS.has(n)) return '';
    const obj = parseToolArgs(args);
    const p = obj.path || obj.file || obj.file_path || obj.filePath || obj.target || '';
    return typeof p === 'string' ? p.trim() : '';
  }

  function displayPath(p) {
    if (!p) return '';
    let out = p;
    if (workspaceRoot) {
      const root = workspaceRoot.replace(/\/+$/, '');
      if (out === root) return '.';
      if (out.startsWith(root + '/')) out = out.slice(root.length + 1);
    }
    return out;
  }

  function changeActionLabel(tool) {
    const n = String(tool || '').toLowerCase();
    if (n === 'write') return 'Wrote';
    if (n === 'edit') return 'Edited';
    return 'Changed';
  }

  function recordFileChange(message, toolName, filePath) {
    if (!message || !filePath) return;
    const n = String(toolName || '').toLowerCase();
    if (!FILE_CHANGE_TOOLS.has(n)) return;
    message.changedFiles = message.changedFiles || [];
    const existing = message.changedFiles.find((f) => f.path === filePath);
    if (existing) {
      existing.action = n;
      return;
    }
    message.changedFiles.push({ path: filePath, action: n });
  }

  function openFilePath(filePath) {
    if (!filePath) return;
    vscode.postMessage({ type: 'openFile', path: filePath });
  }

  function collectChangedFiles(m) {
    if (m?.changedFiles?.length) return m.changedFiles;
    const out = [];
    for (const t of m?.tools || []) {
      const n = String(t.name || '').toLowerCase();
      if (!FILE_CHANGE_TOOLS.has(n)) continue;
      const filePath = t.path || toolFilePath(t.name, t.args);
      if (!filePath) continue;
      const existing = out.find((f) => f.path === filePath);
      if (existing) existing.action = n;
      else out.push({ path: filePath, action: n });
    }
    return out;
  }

  function buildChangesEl(changedFiles) {
    const list = Array.isArray(changedFiles) ? changedFiles : [];
    if (!list.length) return null;
    const wrap = document.createElement('div');
    wrap.className = 'file-changes';
    const head = document.createElement('div');
    head.className = 'file-changes-head';
    head.textContent =
      list.length === 1 ? '1 file changed' : `${list.length} files changed`;
    wrap.appendChild(head);
    const ul = document.createElement('div');
    ul.className = 'file-changes-list';
    for (const f of list) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'file-change';
      btn.title = f.path;
      const action = document.createElement('span');
      action.className = 'file-change-action';
      action.textContent = changeActionLabel(f.action);
      const pathEl = document.createElement('span');
      pathEl.className = 'file-change-path';
      pathEl.textContent = displayPath(f.path);
      btn.appendChild(action);
      btn.appendChild(pathEl);
      btn.addEventListener('click', () => openFilePath(f.path));
      ul.appendChild(btn);
    }
    wrap.appendChild(ul);
    return wrap;
  }

  function buildPlanCard(plan) {
    const card = document.createElement('div');
    card.className = 'plan-card';
    card.dataset.planId = plan.id;
    const heading = document.createElement('div');
    heading.className = 'plan-card-heading';
    const title = document.createElement('strong');
    title.textContent = plan.title;
    const meta = document.createElement('span');
    meta.textContent = `${plan.status} · revision ${plan.revision}`;
    heading.appendChild(title);
    heading.appendChild(meta);
    card.appendChild(heading);

    const actions = document.createElement('div');
    actions.className = 'plan-card-actions';
    const addAction = (label, action, disabled) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'btn btn-secondary';
      button.textContent = label;
      button.disabled = !!disabled;
      button.addEventListener('click', () => {
        vscode.postMessage({
          type: action,
          planId: plan.id,
          path: plan.path,
        });
        setStatus(`${label}…`);
      });
      actions.appendChild(button);
    };
    addAction('Open Plan', 'openPlan');
    addAction(
      'Build',
      'buildPlan',
      plan.status === 'building' || plan.status === 'completed' || plan.status === 'rejected'
    );
    card.appendChild(actions);
    return card;
  }

  function buildSavePlanAction(m) {
    if (!m.offerSavePlan || m.plan || m._planSavePending) return null;
    const row = document.createElement('div');
    row.className = 'plan-save-row';
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'btn btn-secondary';
    button.textContent = 'Save Plan';
    button.addEventListener('click', () => saveMessageAsPlan(m));
    row.appendChild(button);
    return row;
  }

  function saveMessageAsPlan(m) {
    if (!m?.content || m._planSavePending || m.plan) return;
    m._planSavePending = true;
    const stamp = new Date().toISOString().replace('T', ' ').slice(0, 19);
    vscode.postMessage({
      type: 'savePlan',
      messageId: m.id,
      title: `Plan ${stamp}`,
      content: m.content,
      sessionId: state.activeId,
    });
    persistSoon();
    renderMessages();
    setStatus('Saving plan…');
  }

  function renderMessages() {
    const session = active();
    els.messages.innerHTML = '';
    if (!session || session.messages.length === 0) {
      els.empty.style.display = 'block';
      els.messages.appendChild(els.empty);
      return;
    }
    els.empty.style.display = 'none';
    for (const m of session.messages) {
      els.messages.appendChild(buildMessageEl(m));
    }
    els.messages.scrollTop = els.messages.scrollHeight;
  }

  function toolDetail(t) {
    const filePath = t.path || toolFilePath(t.name, t.args);
    if (filePath) return { detail: displayPath(filePath), filePath };
    const obj = parseToolArgs(t.args);
    const cmd = obj.command || obj.cmd || obj.script || '';
    if (cmd) {
      const one = String(cmd).replace(/\s+/g, ' ').trim().slice(0, 72);
      return { detail: one, filePath: '' };
    }
    return { detail: '', filePath: '' };
  }

  function toolsSummary(tools) {
    const list = tools || [];
    if (!list.length) return '';
    const counts = {};
    for (const t of list) {
      const n = String(t.name || 'tool').toLowerCase();
      counts[n] = (counts[n] || 0) + 1;
    }
    return Object.entries(counts)
      .map(([name, n]) => (n > 1 ? `${n}× ${name}` : name))
      .join(' · ');
  }

  function buildActivityEl(m) {
    const hasThinking = !!(m.thinking && (m.thinking.text || !m.thinking.done));
    const tools = m.tools || [];
    if (!hasThinking && !tools.length) return null;

    const wrap = document.createElement('div');
    wrap.className = 'activity';

    if (hasThinking) {
      const thinkingOpen = m.thinking.open === true;
      const think = document.createElement('div');
      think.className =
        'activity-row thinking' +
        (thinkingOpen ? ' open' : '') +
        (m.thinking.done ? ' done' : '');
      const head = document.createElement('button');
      head.type = 'button';
      head.className = 'activity-head';
      head.innerHTML =
        `<span class="activity-label">${m.thinking.done ? 'Thought' : 'Thinking'}</span>` +
        `<span class="chev">▶</span>`;
      const body = document.createElement('div');
      body.className = 'activity-body think-body';
      body.textContent = m.thinking.text || '';
      head.addEventListener('click', () => {
        m.thinking.open = !think.classList.contains('open');
        think.classList.toggle('open');
        persistSoon();
      });
      think.appendChild(head);
      think.appendChild(body);
      wrap.appendChild(think);
    }

    if (tools.length) {
      const open = !!m.toolsOpen;
      const row = document.createElement('div');
      row.className = 'activity-row tools' + (open ? ' open' : '');
      const head = document.createElement('button');
      head.type = 'button';
      head.className = 'activity-head';
      const n = tools.length;
      const summary = toolsSummary(tools);
      const title = m.streaming
        ? `Working · ${summary}`
        : n === 1
          ? `1 step · ${summary}`
          : `${n} steps · ${summary}`;
      head.innerHTML =
        `<span class="activity-label">${escapeHtml(title)}</span>` +
        `<span class="chev">▶</span>`;
      const body = document.createElement('div');
      body.className = 'activity-body steps';
      for (const t of tools) {
        body.appendChild(buildStepEl(m, t));
      }
      head.addEventListener('click', () => {
        m.toolsOpen = !row.classList.contains('open');
        row.classList.toggle('open');
        persistSoon();
      });
      row.appendChild(head);
      row.appendChild(body);
      wrap.appendChild(row);
    }

    return wrap;
  }

  function buildStepEl(m, t) {
    const { detail, filePath } = toolDetail(t);
    const step = document.createElement('div');
    step.className = 'step' + (t.open ? ' open' : '');
    step.dataset.id = t.id;

    const head = document.createElement('button');
    head.type = 'button';
    head.className = 'step-head';
    const name = document.createElement('span');
    name.className = 'step-name';
    name.textContent = t.name || 'tool';
    head.appendChild(name);
    if (detail) {
      const d = document.createElement('span');
      d.className = 'step-detail';
      d.textContent = detail;
      d.title = filePath || detail;
      head.appendChild(d);
    }
    if (filePath) {
      head.addEventListener('dblclick', (e) => {
        e.preventDefault();
        e.stopPropagation();
        openFilePath(filePath);
      });
    }

    const body = document.createElement('div');
    body.className = 'step-body';
    if (filePath) {
      const link = document.createElement('button');
      link.type = 'button';
      link.className = 'file-link';
      link.textContent = displayPath(filePath);
      link.title = filePath;
      link.addEventListener('click', (e) => {
        e.stopPropagation();
        openFilePath(filePath);
      });
      body.appendChild(link);
    }
    const text = t.output || t.args || '';
    if (text) {
      const pre = document.createElement('div');
      pre.className = 'block-body-text';
      pre.textContent = text;
      body.appendChild(pre);
    }

    head.addEventListener('click', () => {
      t.open = !step.classList.contains('open');
      step.classList.toggle('open');
      persistSoon();
    });

    step.appendChild(head);
    step.appendChild(body);
    return step;
  }

  function buildMessageEl(m) {
    const wrap = document.createElement('div');
    wrap.className = `msg ${m.role}`;
    wrap.dataset.id = m.id;

    const header = document.createElement('div');
    header.className = 'msg-header';
    const role = document.createElement('div');
    role.className = 'msg-role';
    role.textContent =
      m.role === 'user' ? 'You' : m.role === 'assistant' ? 'Klepto' : 'System';
    const actions = document.createElement('div');
    actions.className = 'msg-actions';
    const copyBtn = document.createElement('button');
    copyBtn.className = 'icon-btn';
    copyBtn.title = 'Copy';
    copyBtn.textContent = '⧉';
    copyBtn.addEventListener('click', () => copyText(plainText(m)));
    actions.appendChild(copyBtn);
    header.appendChild(role);
    header.appendChild(actions);
    wrap.appendChild(header);

    const activity = buildActivityEl(m);
    if (activity) wrap.appendChild(activity);

    if (m.content != null && m.content !== '') {
      const bubble = document.createElement('div');
      bubble.className = 'bubble' + (m.streaming ? ' streaming' : '');
      if (m.role === 'assistant') {
        bubble.innerHTML = renderMarkdown(m.content);
        wireMarkdownLinks(bubble);
      } else {
        bubble.textContent = String(m.content);
      }
      wrap.appendChild(bubble);
    }

    const changes = buildChangesEl(collectChangedFiles(m));
    if (changes) wrap.appendChild(changes);
    if (m.plan) wrap.appendChild(buildPlanCard(m.plan));
    const savePlan = buildSavePlanAction(m);
    if (savePlan) wrap.appendChild(savePlan);

    return wrap;
  }

  function msgEl(id) {
    return els.messages.querySelector(`.msg[data-id="${id}"]`);
  }

  function persistSoon() {
    clearTimeout(persistSoon._t);
    persistSoon._t = setTimeout(() => persist(), 400);
  }

  function stickScroll() {
    const el = els.messages;
    if (!el) return;
    const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (gap < 96) el.scrollTop = el.scrollHeight;
  }

  function activityToolsTitle(m) {
    const tools = m.tools || [];
    const n = tools.length;
    const summary = toolsSummary(tools);
    if (!n) return '';
    if (m.streaming) return `Working · ${summary}`;
    return n === 1 ? `1 step · ${summary}` : `${n} steps · ${summary}`;
  }

  function ensureActivityShell(wrap, m) {
    let activity = wrap.querySelector('.activity');
    if (!activity) {
      activity = document.createElement('div');
      activity.className = 'activity';
      const header = wrap.querySelector('.msg-header');
      if (header) header.after(activity);
      else wrap.prepend(activity);
    }

    const hasThinking = !!(m.thinking && (m.thinking.text || !m.thinking.done));
    let thinkRow = activity.querySelector('.activity-row.thinking');
    if (hasThinking) {
      if (!thinkRow) {
        thinkRow = document.createElement('div');
        thinkRow.className = 'activity-row thinking';
        const head = document.createElement('button');
        head.type = 'button';
        head.className = 'activity-head';
        head.innerHTML =
          `<span class="activity-label">Thinking</span><span class="chev">▶</span>`;
        const body = document.createElement('div');
        body.className = 'activity-body think-body';
        head.addEventListener('click', () => {
          m.thinking = m.thinking || {};
          m.thinking.open = !thinkRow.classList.contains('open');
          thinkRow.classList.toggle('open');
          persistSoon();
        });
        thinkRow.appendChild(head);
        thinkRow.appendChild(body);
        activity.insertBefore(thinkRow, activity.firstChild);
      }
    } else if (thinkRow) {
      thinkRow.remove();
    }

    const tools = m.tools || [];
    let toolsRow = activity.querySelector('.activity-row.tools');
    if (tools.length) {
      if (!toolsRow) {
        toolsRow = document.createElement('div');
        toolsRow.className = 'activity-row tools';
        const head = document.createElement('button');
        head.type = 'button';
        head.className = 'activity-head';
        head.innerHTML =
          `<span class="activity-label"></span><span class="chev">▶</span>`;
        const body = document.createElement('div');
        body.className = 'activity-body steps';
        head.addEventListener('click', () => {
          m.toolsOpen = !toolsRow.classList.contains('open');
          toolsRow.classList.toggle('open', !!m.toolsOpen);
          if (m.toolsOpen) syncStepsList(m, body);
          persistSoon();
        });
        toolsRow.appendChild(head);
        toolsRow.appendChild(body);
        activity.appendChild(toolsRow);
      }
    } else if (toolsRow) {
      toolsRow.remove();
    }

    if (!activity.children.length) {
      activity.remove();
      return null;
    }
    return activity;
  }

  function syncStepsList(m, bodyEl) {
    const tools = m.tools || [];
    const existing = new Map(
      [...bodyEl.querySelectorAll('.step')].map((el) => [el.dataset.id, el])
    );
    const keep = new Set();
    for (const t of tools) {
      keep.add(t.id);
      let step = existing.get(t.id);
      if (!step) {
        step = buildStepEl(m, t);
        bodyEl.appendChild(step);
      } else {
        // Refresh detail/output without recreating the row.
        const { detail } = toolDetail(t);
        const detailEl = step.querySelector('.step-detail');
        if (detail) {
          if (detailEl) detailEl.textContent = detail;
          else {
            const d = document.createElement('span');
            d.className = 'step-detail';
            d.textContent = detail;
            step.querySelector('.step-head')?.appendChild(d);
          }
        }
        const text = t.output || t.args || '';
        let pre = step.querySelector('.block-body-text');
        if (text) {
          if (!pre) {
            pre = document.createElement('div');
            pre.className = 'block-body-text';
            step.querySelector('.step-body')?.appendChild(pre);
          }
          if (pre.textContent !== text) pre.textContent = text;
        }
        step.classList.toggle('open', !!t.open);
      }
    }
    for (const [id, el] of existing) {
      if (!keep.has(id)) el.remove();
    }
  }

  function patchActivity(id) {
    const session = active();
    const m = session?.messages.find((x) => x.id === id);
    if (!m) return;
    const wrap = msgEl(id);
    if (!wrap) {
      renderMessages();
      return;
    }
    const activity = ensureActivityShell(wrap, m);
    if (!activity) return;

    const thinkRow = activity.querySelector('.activity-row.thinking');
    if (thinkRow && m.thinking) {
      thinkRow.classList.toggle('done', !!m.thinking.done);
      thinkRow.classList.toggle('open', m.thinking.open === true);
      const label = thinkRow.querySelector('.activity-label');
      if (label) label.textContent = m.thinking.done ? 'Thought' : 'Thinking';
      const body = thinkRow.querySelector('.think-body');
      // Only write when open — avoids layout thrash while collapsed.
      if (body && m.thinking.open === true && body.textContent !== (m.thinking.text || '')) {
        body.textContent = m.thinking.text || '';
      }
    }

    const toolsRow = activity.querySelector('.activity-row.tools');
    if (toolsRow) {
      toolsRow.classList.toggle('open', !!m.toolsOpen);
      const label = toolsRow.querySelector('.activity-label');
      const title = activityToolsTitle(m);
      if (label && label.textContent !== title) label.textContent = title;
      if (m.toolsOpen) {
        const body = toolsRow.querySelector('.steps');
        if (body) syncStepsList(m, body);
      }
    }
  }

  function schedulePatchActivity(id) {
    if (!id) return;
    schedulePatchActivity._ids = schedulePatchActivity._ids || new Set();
    schedulePatchActivity._ids.add(id);
    if (schedulePatchActivity._raf) return;
    schedulePatchActivity._raf = requestAnimationFrame(() => {
      schedulePatchActivity._raf = null;
      const ids = schedulePatchActivity._ids;
      schedulePatchActivity._ids = new Set();
      for (const mid of ids) patchActivity(mid);
    });
  }

  function patchBubble(id, opts) {
    const finalize = !!(opts && opts.finalize);
    const session = active();
    const m = session?.messages.find((x) => x.id === id);
    if (!m) return;
    const wrap = msgEl(id);
    if (!wrap) {
      renderMessages();
      return;
    }
    let bubble = wrap.querySelector('.bubble');
    if (!bubble) {
      bubble = document.createElement('div');
      bubble.className = 'bubble';
      const changes = wrap.querySelector('.file-changes');
      if (changes) wrap.insertBefore(bubble, changes);
      else wrap.appendChild(bubble);
    }

    const streaming = !!m.streaming && !finalize;
    bubble.classList.toggle('streaming', streaming);

    if (m.role !== 'assistant') {
      const next = String(m.content || '');
      if (bubble.textContent !== next) bubble.textContent = next;
      stickScroll();
      return;
    }

    // While streaming: plain text only (no markdown rebuild / flicker).
    // Finalize once at the end with markdown.
    if (streaming) {
      let plain = bubble.querySelector('.stream-plain');
      if (!plain || bubble.dataset.mode !== 'plain') {
        bubble.textContent = '';
        plain = document.createElement('div');
        plain.className = 'stream-plain md-p';
        bubble.appendChild(plain);
        bubble.dataset.mode = 'plain';
      }
      const next = String(m.content || '');
      if (plain.textContent !== next) plain.textContent = next;
    } else {
      if (bubble.dataset.mode !== 'md' || bubble.dataset.len !== String((m.content || '').length)) {
        bubble.innerHTML = renderMarkdown(m.content || '');
        wireMarkdownLinks(bubble);
        bubble.dataset.mode = 'md';
        bubble.dataset.len = String((m.content || '').length);
      }
    }
    stickScroll();
  }

  function patchChanges(id) {
    const session = active();
    const m = session?.messages.find((x) => x.id === id);
    if (!m) return;
    const files = collectChangedFiles(m);
    if (!files.length) return; // don't touch DOM until something changed
    const wrap = msgEl(id);
    if (!wrap) return;
    const next = buildChangesEl(files);
    const prev = wrap.querySelector('.file-changes');
    if (!next) return;
    // Update in place when count/paths unchanged shape.
    if (prev) {
      const head = prev.querySelector('.file-changes-head');
      const label =
        files.length === 1 ? '1 file changed' : `${files.length} files changed`;
      if (head && head.textContent !== label) head.textContent = label;
      const list = prev.querySelector('.file-changes-list');
      if (list && list.children.length !== files.length) {
        prev.replaceWith(next);
      }
    } else {
      wrap.appendChild(next);
    }
  }

  function plainText(m) {
    const parts = [];
    if (m.thinking?.text) parts.push(`[thinking]\n${m.thinking.text}`);
    if (m.tools) {
      for (const t of m.tools) {
        parts.push(`[tool:${t.name}]\n${t.output || t.args || ''}`);
      }
    }
    if (m.content) parts.push(String(m.content));
    return parts.join('\n\n');
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(text);
      setStatus('Copied');
      setTimeout(() => setStatus(''), 1200);
    } catch {
      vscode.postMessage({ type: 'copyText', text });
    }
  }

  function uid(prefix) {
    return `${prefix}_${Math.random().toString(36).slice(2, 9)}`;
  }

  function addMessage(partial) {
    const session = active();
    if (!session) return null;
    const msg = {
      id: partial.id || uid('m'),
      role: partial.role,
      content: partial.content ?? '',
      createdAt: Date.now(),
      streaming: !!partial.streaming,
      thinking: partial.thinking || null,
      tools: partial.tools || [],
    };
    session.messages.push(msg);
    if (session.messages.length === 1 && msg.role === 'user') {
      session.title = String(msg.content).slice(0, 28) || session.title;
      renderTabs();
    }
    persistSoon();
    // Append a single node when DOM is in sync — avoid full list rebuild.
    if (els.empty) els.empty.style.display = 'none';
    const mounted = els.messages.querySelectorAll('.msg').length;
    if (mounted === session.messages.length - 1) {
      if (mounted === 0) {
        // Drop empty-state node if it's the only child.
        els.messages.innerHTML = '';
      }
      els.messages.appendChild(buildMessageEl(msg));
      stickScroll();
    } else {
      renderMessages();
    }
    return msg;
  }

  function updateMessage(id, patch) {
    const session = active();
    if (!session) return;
    const msg = session.messages.find((m) => m.id === id);
    if (!msg) return;
    const wasStreaming = !!msg.streaming;
    Object.assign(msg, patch);
    if (patch.thinking) msg.thinking = { ...msg.thinking, ...patch.thinking };
    persistSoon();
    if (msgEl(id)) {
      if (patch.thinking || patch.tools || patch.toolsOpen != null) {
        schedulePatchActivity(id);
      }
      if (patch.content != null || patch.streaming != null) {
        const ending = wasStreaming && patch.streaming === false;
        patchBubble(id, { finalize: ending || patch.streaming === false });
      }
      if (patch.changedFiles) patchChanges(id);
    } else {
      renderMessages();
    }
  }

  function appendThinking(text) {
    const session = active();
    const streamingMsgId = session?.streamingMsgId;
    if (!text || !streamingMsgId) return;
    const m = session.messages.find((x) => x.id === streamingMsgId);
    if (!m) return;
    m.thinking = m.thinking || { id: uid('think'), text: '', done: false, open: false };
    const prev = (m.thinking.text || '').trim();
    // Replace the extension placeholder once real harness thinking arrives.
    if (!prev || prev === 'Planning response…') {
      m.thinking.text = text;
    } else {
      m.thinking.text += text;
    }
    m.thinking.done = false;
    // Keep collapsed by default — expand only if user opened it.
    if (m.thinking.open == null) m.thinking.open = false;
    session.thinkingBlockId = m.thinking.id;
    persistSoon();
    schedulePatchActivity(streamingMsgId);
  }

  function appendToMessage(id, text) {
    const session = active();
    if (!session) return;
    const msg = session.messages.find((m) => m.id === id);
    if (!msg) return;
    msg.content = (msg.content || '') + text;
    persistSoon();
    if (!appendToMessage._raf) {
      appendToMessage._raf = requestAnimationFrame(() => {
        appendToMessage._raf = null;
        patchBubble(id);
      });
    }
  }

  function switchSession(id) {
    if (!state.sessions[id]) return;
    state.activeId = id;
    persist();
    renderTabs();
    renderMessages();
    syncGeneratingUi();
    vscode.postMessage({ type: 'switchSession', sessionId: id });
  }

  function closeSession(id) {
    removeSessionState(id);
    renderTabs();
    renderMessages();
    syncGeneratingUi();
    vscode.postMessage({ type: 'closeSession', sessionId: id });
    if (state.activeId) {
      vscode.postMessage({ type: 'switchSession', sessionId: state.activeId });
    }
  }

  function removeSessionState(id) {
    const idx = state.tabOrder.indexOf(id);
    if (idx >= 0) state.tabOrder.splice(idx, 1);
    delete state.sessions[id];
    if (state.activeId === id) {
      state.activeId = state.tabOrder[idx] || state.tabOrder[idx - 1] || state.tabOrder[0] || null;
    }
    persist();
  }

  function exportTranscript(format) {
    const session = active();
    if (!session) {
      setStatus('No session to export');
      return;
    }
    let content = '';
    if (format === 'json') {
      content = JSON.stringify(session, null, 2);
    } else {
      const lines = [`# ${session.title}`, '', `Session: ${session.id}`, ''];
      for (const m of session.messages) {
        const role = m.role === 'user' ? 'You' : m.role === 'assistant' ? 'Klepto' : 'System';
        lines.push(`## ${role}`, '');
        if (m.thinking?.text) {
          lines.push('<details><summary>Thinking</summary>', '', m.thinking.text, '', '</details>', '');
        }
        if (m.content) lines.push(String(m.content), '');
      }
      content = lines.join('\n');
    }
    vscode.postMessage({
      type: 'exportTranscript',
      format,
      sessionId: session.id,
      title: session.title,
      content,
    });
  }

  function autoGrow() {
    els.input.style.height = 'auto';
    els.input.style.height = Math.min(els.input.scrollHeight, 140) + 'px';
  }

  function composerPlainText() {
    return (els.input.innerText || '').replace(/\u00a0/g, ' ').trim();
  }

  function clearComposer() {
    els.input.innerHTML = '';
    pendingAttachments = [];
    renderAttachRow();
    autoGrow();
    updatePlaceholder();
  }

  function updatePlaceholder() {
    const empty = !composerPlainText() && !els.input.querySelector('.pill');
    els.input.classList.toggle('is-empty', empty);
  }

  function isImage(mime) {
    return !!(mime && mime.startsWith('image/'));
  }
  function fileIconFor(mime, name) {
    if (isImage(mime)) return '🖼';
    if (/\.pdf$/i.test(name || '')) return '📄';
    if (/\.(zip|tar|gz|bz2|7z|rar)$/i.test(name || '')) return '📦';
    if (/\.(json|yaml|yml|toml|csv|txt|log)$/i.test(name || '')) return '📋';
    return '📎';
  }
  function formatFileSize(bytes) {
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
  }
  function renderAttachRow() {
    if (!els.attachRow) return;
    if (!pendingAttachments.length) {
      els.attachRow.hidden = true;
      els.attachRow.innerHTML = '';
      return;
    }
    els.attachRow.hidden = false;
    els.attachRow.innerHTML = '';
    for (const a of pendingAttachments) {
      const card = document.createElement('div');
      card.className = 'attach-card' + (isImage(a.mime) ? ' is-image' : '');
      if (isImage(a.mime)) {
        const thumb = document.createElement('div');
        thumb.className = 'attach-card-thumb';
        const img = document.createElement('img');
        img.alt = a.name || '';
        img.draggable = false;
        const blobUrl = a._blobUrl;
        if (blobUrl) {
          img.src = blobUrl;
          img.onload = () => {
            const r = img.naturalWidth / img.naturalHeight;
            thumb.style.aspectRatio = r > 1 ? '16/9' : '9/16';
          };
          img.onerror = () => { thumb.style.display = 'none'; };
        } else {
          img.style.display = 'none';
        }
        thumb.appendChild(img);
        card.appendChild(thumb);
        const info = document.createElement('div');
        info.className = 'attach-card-info';
        const name = document.createElement('div');
        name.className = 'attach-card-name';
        name.textContent = a.name || 'image';
        name.title = a.name || a.path;
        const meta = document.createElement('div');
        meta.className = 'attach-card-meta';
        meta.textContent = (a.mime || '').replace('image/', '').toUpperCase();
        info.appendChild(name);
        info.appendChild(meta);
        card.appendChild(info);
      } else {
        const icon = document.createElement('div');
        icon.className = 'attach-card-icon';
        icon.textContent = fileIconFor(a.mime, a.name);
        card.appendChild(icon);
        const info = document.createElement('div');
        info.className = 'attach-card-info';
        const name = document.createElement('div');
        name.className = 'attach-card-name';
        name.textContent = a.name || a.path.split(/[/\\]/).pop() || 'file';
        name.title = a.name || a.path;
        const meta = document.createElement('div');
        meta.className = 'attach-card-meta';
        meta.textContent = a.mime || 'file';
        info.appendChild(name);
        info.appendChild(meta);
        card.appendChild(info);
      }
      const rm = document.createElement('button');
      rm.type = 'button';
      rm.className = 'attach-card-remove';
      rm.innerHTML = '<svg width="10" height="10" viewBox="0 0 10 10" fill="none"><path d="M2.5 2.5l5 5M7.5 2.5l-5 5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';
      rm.title = 'Remove';
      rm.addEventListener('click', () => {
        if (a._blobUrl) URL.revokeObjectURL(a._blobUrl);
        pendingAttachments = pendingAttachments.filter((x) => x.path !== a.path);
        renderAttachRow();
      });
      card.appendChild(rm);
      els.attachRow.appendChild(card);
    }
  }

  function serializeComposer() {
    const mentions = [];
    const urls = [];
    let text = '';

    function walk(node) {
      if (node.nodeType === Node.TEXT_NODE) {
        text += node.textContent || '';
        return;
      }
      if (node.nodeType !== Node.ELEMENT_NODE) return;
      const el = node;
      if (el.classList.contains('pill-url')) {
        const url = el.getAttribute('data-url') || '';
        const docPath = el.getAttribute('data-path') || '';
        const title = el.getAttribute('data-title') || '';
        text += url;
        urls.push({
          url,
          doc_path: docPath || undefined,
          title: title || undefined,
        });
        return;
      }
      if (el.classList.contains('pill-mention')) {
        const path = el.getAttribute('data-path') || '';
        const kind = el.getAttribute('data-kind') || 'file';
        const label = el.getAttribute('data-label') || '';
        text += `@${label || path}`;
        mentions.push({ kind, path, label: label || undefined });
        return;
      }
      if (el.tagName === 'BR') {
        text += '\n';
        return;
      }
      if (el.tagName === 'DIV' || el.tagName === 'P') {
        if (text && !text.endsWith('\n')) text += '\n';
      }
      for (const child of el.childNodes) walk(child);
    }

    for (const child of els.input.childNodes) walk(child);
    return {
      text: text.replace(/\u00a0/g, ' ').trim(),
      mentions,
      urls,
      attachments: pendingAttachments.slice(),
    };
  }

  function insertNodeAtCursor(node) {
    const sel = window.getSelection();
    if (!sel || !sel.rangeCount) {
      els.input.appendChild(node);
      return;
    }
    const range = sel.getRangeAt(0);
    if (!els.input.contains(range.commonAncestorContainer)) {
      els.input.appendChild(node);
      return;
    }
    range.deleteContents();
    range.insertNode(node);
    range.setStartAfter(node);
    range.collapse(true);
    sel.removeAllRanges();
    sel.addRange(range);
  }

  function placeCaretAfter(node) {
    const sel = window.getSelection();
    if (!sel) return;
    const range = document.createRange();
    range.setStartAfter(node);
    range.collapse(true);
    sel.removeAllRanges();
    sel.addRange(range);
  }

  function createUrlPill(url, opts) {
    const pill = document.createElement('span');
    pill.className = 'pill pill-url' + (opts && opts.loading ? ' is-loading' : '');
    pill.contentEditable = 'false';
    pill.setAttribute('data-url', url);
    if (opts && opts.path) pill.setAttribute('data-path', opts.path);
    if (opts && opts.title) pill.setAttribute('data-title', opts.title);
    const icon = document.createElement('span');
    icon.className = 'pill-icon';
    icon.setAttribute('aria-hidden', 'true');
    icon.innerHTML =
      '<svg width="10" height="10" viewBox="0 0 16 16" fill="none"><path d="M6.5 9.5l3-3M7 12.5l-.5.5a3 3 0 01-4.2-4.2L4 7m5-3.5l.5-.5a3 3 0 014.2 4.2L12 9" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/></svg>';
    const label = document.createElement('span');
    label.className = 'pill-label';
    try {
      const u = new URL(url);
      label.textContent = u.host + (u.pathname === '/' ? '' : u.pathname);
    } catch {
      label.textContent = url;
    }
    pill.appendChild(icon);
    pill.appendChild(label);
    return pill;
  }

  function createMentionPill(candidate) {
    const pill = document.createElement('span');
    pill.className = 'pill pill-mention';
    pill.contentEditable = 'false';
    pill.setAttribute('data-kind', candidate.kind || 'file');
    pill.setAttribute('data-path', candidate.path);
    pill.setAttribute('data-label', candidate.label || candidate.path);
    const icon = document.createElement('span');
    icon.className = 'pill-icon';
    icon.setAttribute('aria-hidden', 'true');
    icon.textContent = candidate.kind === 'doc' ? 'doc' : '@';
    const label = document.createElement('span');
    label.className = 'pill-label';
    label.textContent = candidate.label || candidate.path.split(/[/\\]/).pop();
    pill.appendChild(icon);
    pill.appendChild(label);
    return pill;
  }

  function startUrlFetch(pill, url) {
    const requestId = nextRequestId('fetch');
    pendingFetches.set(requestId, pill);
    pill.classList.add('is-loading');
    vscode.postMessage({ type: 'fetchUrl', url, requestId });
  }

  function insertUrlPill(url) {
    const pill = createUrlPill(url, { loading: true });
    insertNodeAtCursor(pill);
    const space = document.createTextNode('\u00a0');
    placeCaretAfter(pill);
    insertNodeAtCursor(space);
    placeCaretAfter(space);
    startUrlFetch(pill, url);
    updatePlaceholder();
    autoGrow();
  }

  function insertMentionPill(candidate) {
    // Remove trailing @query text before cursor
    const sel = window.getSelection();
    if (sel && sel.rangeCount) {
      const range = sel.getRangeAt(0);
      const node = range.startContainer;
      if (node.nodeType === Node.TEXT_NODE) {
        const text = node.textContent || '';
        const before = text.slice(0, range.startOffset);
        const at = before.lastIndexOf('@');
        if (at >= 0) {
          node.textContent = text.slice(0, at) + text.slice(range.startOffset);
          range.setStart(node, at);
          range.collapse(true);
          sel.removeAllRanges();
          sel.addRange(range);
        }
      }
    }
    const pill = createMentionPill(candidate);
    insertNodeAtCursor(pill);
    const space = document.createTextNode('\u00a0');
    placeCaretAfter(pill);
    insertNodeAtCursor(space);
    placeCaretAfter(space);
    hideMentionMenu();
    updatePlaceholder();
    autoGrow();
  }

  function hideMentionMenu() {
    mentionState.open = false;
    mentionState.candidates = [];
    if (els.mentionMenu) {
      els.mentionMenu.hidden = true;
      els.mentionMenu.innerHTML = '';
    }
  }

  function renderMentionMenu() {
    if (!els.mentionMenu) return;
    const items = mentionState.candidates;
    if (!mentionState.open || !items.length) {
      els.mentionMenu.hidden = true;
      return;
    }
    els.mentionMenu.hidden = false;
    els.mentionMenu.innerHTML = '';
    items.forEach((c, i) => {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'mention-item' + (i === mentionState.index ? ' active' : '');
      btn.innerHTML =
        `<span class="mention-kind">${c.kind === 'doc' ? 'doc' : 'file'}</span>` +
        `<span class="mention-label">${escapeHtml(c.label)}</span>` +
        (c.detail ? `<span class="mention-detail">${escapeHtml(c.detail)}</span>` : '');
      btn.addEventListener('mousedown', (e) => {
        e.preventDefault();
        insertMentionPill(c);
      });
      els.mentionMenu.appendChild(btn);
    });
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function getAtQuery() {
    const sel = window.getSelection();
    if (!sel || !sel.rangeCount) return null;
    const range = sel.getRangeAt(0);
    if (!els.input.contains(range.startContainer)) return null;
    const node = range.startContainer;
    if (node.nodeType !== Node.TEXT_NODE) return null;
    const before = (node.textContent || '').slice(0, range.startOffset);
    const match = before.match(/@([^\s@]*)$/);
    if (!match) return null;
    return { query: match[1] || '', atIndex: before.length - match[0].length };
  }

  function requestMentions(query) {
    const requestId = nextRequestId('mention');
    mentionState.requestId = requestId;
    mentionState.query = query;
    mentionState.open = true;
    mentionState.index = 0;
    vscode.postMessage({ type: 'searchMentions', query, requestId });
  }

  function onComposerInput() {
    updatePlaceholder();
    autoGrow();
    const at = getAtQuery();
    if (at) {
      requestMentions(at.query);
    } else {
      hideMentionMenu();
    }
  }

  const URL_RE = /https?:\/\/[^\s<>"')\]]+/i;

  function onComposerPaste(e) {
    const dt = e.clipboardData;
    if (!dt) return;

    // Image paste
    const items = [...(dt.items || [])];
    const imageItem = items.find((it) => it.type && it.type.startsWith('image/'));
    if (imageItem) {
      e.preventDefault();
      const file = imageItem.getAsFile();
      if (file) attachBlob(file);
      return;
    }

    // Other file types from clipboard (e.g. file copied from Finder)
    const clipItems = [...(dt.items || [])].filter((it) => it.kind === 'file');
    if (clipItems.length) {
      e.preventDefault();
      for (const item of clipItems) {
        const file = item.getAsFile();
        if (file) attachBlob(file);
      }
      return;
    }

    const text = dt.getData('text/plain') || '';
    const trimmed = text.trim();
    if (URL_RE.test(trimmed) && /^https?:\/\/\S+$/i.test(trimmed)) {
      e.preventDefault();
      insertUrlPill(trimmed.replace(/[.,;:!?)]+$/, ''));
      return;
    }
  }

  function attachBlob(file) {
    const name = file.name || 'untitled';
    const mime = file.type || 'application/octet-stream';
    const previewUrl = isImage(mime) ? URL.createObjectURL(file) : null;
    const entry = { path: `pending-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, name, mime, _blobUrl: previewUrl };
    pendingAttachments.push(entry);
    renderAttachRow();
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result || '');
      const base64 = result.includes(',') ? result.split(',')[1] : result;
      const requestId = nextRequestId('attach');
      pendingRequests.set(requestId, 'attach');
      // Update entry path once we have a real requestId
      entry.path = requestId;
      vscode.postMessage({
        type: 'attachFiles',
        requestId,
        sessionId: state.activeId || 'pending',
        name: name,
        mime: mime,
        dataBase64: base64,
      });
    };
    reader.readAsDataURL(file);
  }

  function openAttachPicker() {
    const requestId = nextRequestId('attach');
    pendingRequests.set(requestId, 'attach');
    vscode.postMessage({
      type: 'attachFiles',
      requestId,
      sessionId: state.activeId || 'pending',
    });
  }

  const PLAN_CONTINUE_MAX = 2;
  const PLAN_CONTINUE_PROMPT =
    'Continue exploring with read-only tools and finish with one Markdown plan. Start with YAML frontmatter containing `name`, `overview`, `todos` (stable id, content, pending status), and `isProject: false`, then include the implementation plan body. ' +
    'Do not stop or ask questions unless a real decision from me is required to proceed.';

  function sessionMode() {
    const s = active();
    const profile = String(s?.profile || state.prefs.profile || '').toLowerCase();
    if (profile === 'plan' || profile === 'debug') return profile;
    return String(s?.agentMode || s?.agent_mode || state.prefs.agentMode || 'agent').toLowerCase();
  }

  function offerSavePlan(finishedMsg) {
    if (!finishedMsg || sessionMode() !== 'plan' || !finishedMsg.content?.trim()) return;
    finishedMsg.offerSavePlan = true;
    persistSoon();
    renderMessages();
  }

  function looksLikeFinishedPlan(text) {
    const t = String(text || '');
    if (!t.trim()) return false;
    const hasHeading = /^#{0,3}\s*Plan\s*:/im.test(t) || /\bPlan\s*:\s*$/im.test(t);
    const hasSteps = /^\s*\d+\.\s+\S/m.test(t);
    return hasHeading && hasSteps;
  }

  function looksLikeStructuredPlan(text) {
    const t = String(text || '').trim();
    return (
      t.startsWith('---') &&
      /^name:\s*\S/im.test(t) &&
      /^overview:\s*\S/im.test(t) &&
      /^todos:\s*$/im.test(t) &&
      /^\s*-\s+id:\s*\S/im.test(t) &&
      /^isProject:\s*(true|false)\s*$/im.test(t)
    );
  }

  function looksLikeNeedsUserDecision(text) {
    const t = String(text || '').trim();
    if (!t) return false;
    const askRe =
      /\b(should i|shall i|do you want|would you like|which (one|approach|option)|pick one|please confirm|confirm that|approve|your (call|choice|preference)|let me know|need (your|a) (decision|answer|choice)|what do you (want|prefer))\b/i;
    if (!askRe.test(t)) return false;
    const tail = t.slice(-500);
    return /\?/.test(tail) || askRe.test(tail);
  }

  function maybeContinuePlan(finishedMsg) {
    if (!finishedMsg || sessionMode() !== 'plan') return;
    if (finishedMsg._harnessError || finishedMsg._stopped) return;
    if (finishedMsg._planContinueChecked) return;
    finishedMsg._planContinueChecked = true;

    const session = active();
    if (!session) return;
    if (typeof session.planContinues !== 'number') session.planContinues = 0;

    const content = finishedMsg.content || '';
    if (looksLikeStructuredPlan(content)) {
      session.planContinues = 0;
      saveMessageAsPlan(finishedMsg);
      persistSoon();
      return;
    }
    if (looksLikeFinishedPlan(content)) {
      session.planContinues = 0;
      offerSavePlan(finishedMsg);
      persistSoon();
      return;
    }
    if (looksLikeNeedsUserDecision(content)) {
      session.planContinues = 0;
      persistSoon();
      setStatus('Waiting for your decision…');
      return;
    }
    if (session.planContinues >= PLAN_CONTINUE_MAX) {
      offerSavePlan(finishedMsg);
      setStatus('Plan paused — send a message to continue');
      return;
    }

    session.planContinues += 1;
    persistSoon();
    setStatus('Continuing plan…');
    clearTimeout(planContinueTimers.get(session.id));
    planContinueTimers.set(
      session.id,
      setTimeout(() => autoPromptPlanContinue(session.id), 400)
    );
  }

  function autoPromptPlanContinue(sessionId) {
    planContinueTimers.delete(sessionId);
    if (state.activeId !== sessionId || isGenerating() || sessionMode() !== 'plan') return;
    const session = active();
    if (!session) return;
    addMessage({ role: 'system', content: 'Continuing plan…' });
    const assistant = addMessage({
      role: 'assistant',
      content: '',
      streaming: true,
      thinking: { id: uid('think'), text: '', done: false, open: false },
    });
    session.streamingMsgId = assistant.id;
    session.thinkingBlockId = assistant.thinking.id;
    setGenerating(true);
    setStatus('Continuing plan…');
    vscode.postMessage({
      type: 'sendMessage',
      message: PLAN_CONTINUE_PROMPT,
      sessionId: state.activeId,
    });
  }

  function send() {
    const payload = serializeComposer();
    if ((!payload.text && !payload.attachments.length) || isGenerating()) return;
    const text = payload.text || '(attachments)';
    if (!state.activeId) {
      els.input.dataset.pendingSend = JSON.stringify(payload);
      vscode.postMessage({ type: 'createSession', thenSend: text });
      clearComposer();
      setStatus('Starting session…');
      return;
    }
    const session = active();
    if (session) session.planContinues = 0;
    clearTimeout(planContinueTimers.get(session?.id));
    if (session) planContinueTimers.delete(session.id);
    addMessage({ role: 'user', content: text });
    const assistant = addMessage({
      role: 'assistant',
      content: '',
      streaming: true,
      thinking: { id: uid('think'), text: '', done: false, open: true },
    });
    session.streamingMsgId = assistant.id;
    session.thinkingBlockId = assistant.thinking.id;
    setGenerating(true);
    setStatus('Thinking…');
    vscode.postMessage({
      type: 'sendMessage',
      message: text,
      sessionId: state.activeId,
      mentions: payload.mentions,
      attachments: payload.attachments,
      urls: payload.urls,
    });
    clearComposer();
  }

  function stop() {
    const session = active();
    if (!session || !session.generating || session.stopPending) return;
    clearTimeout(planContinueTimers.get(session.id));
    planContinueTimers.delete(session.id);
    session.stopPending = true;
    session.ignoreAbortError = true;
    const response = session.messages.find((message) => message.id === session.streamingMsgId);
    if (response) response._stopped = true;
    setGenerating(true);
    setStatus('Stopping…');
    vscode.postMessage({ type: 'stopSession', sessionId: session.id });
  }

  // Events
  els.send.addEventListener('click', () => {
    if (isGenerating()) stop();
    else send();
  });
  function requestNewChatTab() {
    if (newSessionPending) return;
    newSessionPending = true;
    setTimeout(() => {
      newSessionPending = false;
    }, 1500);
    vscode.postMessage({ type: 'createSession' });
    setStatus('Starting session…');
  }

  els.newSession.addEventListener('click', () => {
    requestNewChatTab();
  });

  // Chat shortcuts are captured here because host keybindings can miss webview focus.
  window.addEventListener(
    'keydown',
    (e) => {
      if (e.key === 'Escape' && isGenerating()) {
        e.preventDefault();
        e.stopPropagation();
        stop();
        return;
      }
      if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey) return;
      const key = e.key.toLowerCase();
      if (key !== 't' && key !== 'n') return;
      e.preventDefault();
      e.stopPropagation();
      requestNewChatTab();
    },
    true
  );
  if (els.attachBtn) {
    els.attachBtn.addEventListener('click', openAttachPicker);
  }
  els.input.addEventListener('input', onComposerInput);
  els.input.addEventListener('paste', onComposerPaste);
  // Drag-and-drop: accept files on the whole composer box.
  // Listeners live on the box itself: the .drop-zone overlay is
  // pointer-events:none (purely visual), so it can never receive events.
  function dragHasFiles(e) {
    const types = e.dataTransfer?.types;
    return !!(types && Array.from(types).includes('Files'));
  }
  function setDragActive(active) {
    els.dropZone?.classList.toggle('is-active', active);
    els.composerBox?.classList.toggle('is-dragging', active);
  }
  function onDragEnter(e) {
    if (!dragHasFiles(e)) return;
    e.preventDefault();
    if (!els.dropZone?.classList.contains('is-active')) setDragActive(true);
  }
  function onDragOver(e) {
    if (!dragHasFiles(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
  }
  function onDragLeave(e) {
    if (!dragHasFiles(e)) return;
    const box = els.composerBox || els.dropZone;
    if (!box) return;
    // Only close when we actually leave the box, not when moving between children.
    const rect = box.getBoundingClientRect();
    if (e.clientX < rect.left || e.clientX >= rect.right || e.clientY < rect.top || e.clientY >= rect.bottom) {
      setDragActive(false);
    }
  }
  function onDrop(e) {
    if (!dragHasFiles(e)) return;
    e.preventDefault();
    setDragActive(false);
    const files = [...(e.dataTransfer?.files || [])];
    for (const f of files) attachBlob(f);
  }
  const dragTarget = els.composerBox || els.dropZone;
  if (dragTarget) {
    dragTarget.addEventListener('dragenter', onDragEnter);
    dragTarget.addEventListener('dragover', onDragOver);
    dragTarget.addEventListener('dragleave', onDragLeave);
    dragTarget.addEventListener('drop', onDrop);
  }
  function handleComposerEnter(e) {
    if (mentionState.open && els.mentionMenu && !els.mentionMenu.hidden) {
      return false;
    }
    if ((e.key === 'Enter' || e.key === 'NumpadEnter') && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      e.stopPropagation();
      if (isGenerating()) stop();
      else send();
      return true;
    }
    return false;
  }
  els.input.addEventListener(
    'keydown',
    (e) => {
      if (mentionState.open && els.mentionMenu && !els.mentionMenu.hidden) {
        if (e.key === 'ArrowDown') {
          e.preventDefault();
          mentionState.index = Math.min(
            mentionState.index + 1,
            Math.max(0, mentionState.candidates.length - 1)
          );
          renderMentionMenu();
          return;
        }
        if (e.key === 'ArrowUp') {
          e.preventDefault();
          mentionState.index = Math.max(mentionState.index - 1, 0);
          renderMentionMenu();
          return;
        }
        if (e.key === 'Enter' || e.key === 'Tab') {
          e.preventDefault();
          const c = mentionState.candidates[mentionState.index];
          if (c) insertMentionPill(c);
          return;
        }
        if (e.key === 'Escape') {
          e.preventDefault();
          hideMentionMenu();
          return;
        }
      }
      handleComposerEnter(e);
    },
    true
  );
  els.input.addEventListener('beforeinput', (e) => {
    if (e.isComposing) return;
    if (mentionState.open && els.mentionMenu && !els.mentionMenu.hidden) return;
    // Contenteditable: Enter → insertParagraph; keep Shift+Enter as newline.
    if (e.inputType === 'insertParagraph' && !e.shiftKey) {
      e.preventDefault();
      if (isGenerating()) stop();
      else send();
    }
  });
  els.modeToggle.addEventListener('click', (e) => {
    const btn = e.target.closest('.mode-btn');
    if (!btn) return;
    setMode(btn.getAttribute('data-mode') || 'agent');
    fillProfiles(profileCatalog);
  });
  els.profileSelect.addEventListener('change', () => {
    const profile = els.profileSelect.value || 'coding';
    state.prefs.profile = profile;
    if (profile === 'plan' || profile === 'debug') {
      state.prefs.agentMode = profile;
    } else if (profile === 'coding') {
      state.prefs.agentMode = 'agent';
    }
    for (const btn of els.modeToggle.querySelectorAll('.mode-btn')) {
      btn.classList.toggle(
        'active',
        btn.getAttribute('data-mode') === state.prefs.agentMode
      );
    }
    persist();
    postPrefs();
  });
  els.providerSelect.addEventListener('change', () => {
    state.prefs.provider = els.providerSelect.value;
    // Clear model if it no longer belongs to provider
    if (state.prefs.model && state.prefs.provider) {
      const stillValid = modelCatalog.some(
        (m) => m.label === state.prefs.model && m.provider === state.prefs.provider
      );
      if (!stillValid) state.prefs.model = '';
    }
    fillModels();
    persist();
    postPrefs();
  });
  els.modelSelect.addEventListener('change', () => {
    state.prefs.model = els.modelSelect.value;
    if (state.prefs.model && state.prefs.model.includes('/')) {
      const p = state.prefs.model.split('/')[0];
      if (p && !state.prefs.provider) {
        state.prefs.provider = p;
        fillProviders([
          ...new Set([
            ...[...els.providerSelect.options].map((o) => o.value).filter(Boolean),
            p,
          ]),
        ]);
        els.providerSelect.value = p;
      }
    }
    persist();
    postPrefs();
  });
  els.moreBtn.addEventListener('click', (e) => {
    e.stopPropagation();
    els.moreMenu.classList.toggle('open');
  });
  document.addEventListener('click', () => els.moreMenu.classList.remove('open'));
  els.moreMenu.addEventListener('click', (e) => {
    const action = e.target.getAttribute('data-action');
    if (!action) return;
    els.moreMenu.classList.remove('open');
    if (action === 'export-md') exportTranscript('md');
    if (action === 'export-json') exportTranscript('json');
    if (action === 'copy-all') {
      const session = active();
      if (session) copyText(session.messages.map(plainText).join('\n\n---\n\n'));
    }
    if (action === 'clear') {
      const session = active();
      if (session) {
        session.messages = [];
        persist();
        renderMessages();
      }
    }
    if (action === 'refresh-models') {
      vscode.postMessage({ type: 'refreshModels' });
      setStatus('Refreshing models…');
    }
  });

  window.addEventListener('message', (event) => {
    const msg = event.data;
    switch (msg.type) {
      case 'focusComposer':
        els.input.focus();
        break;

      case 'connectionStatus':
        setConnected(!!msg.connected);
        break;

      case 'workspaceInfo':
        workspaceRoot = msg.root || '';
        break;

      case 'modelsUpdate':
        applyModelsUpdate(msg);
        break;

      case 'profilesUpdate':
        applyProfilesUpdate(msg);
        break;

      case 'requestCreatePlan': {
        const messages = active()?.messages || [];
        const latest = [...messages]
          .reverse()
          .find((m) => m.role === 'assistant' && m.content?.trim());
        if (latest) saveMessageAsPlan(latest);
        else setStatus('No assistant plan to save');
        break;
      }

      case 'fetchUrlResult': {
        const pill = pendingFetches.get(msg.requestId);
        pendingFetches.delete(msg.requestId);
        if (!pill) break;
        pill.classList.remove('is-loading');
        if (msg.ok) {
          if (msg.path) pill.setAttribute('data-path', msg.path);
          if (msg.title) pill.setAttribute('data-title', msg.title);
          pill.classList.add('is-ready');
          setStatus(`Indexed ${msg.title || msg.url}`);
        } else {
          pill.classList.add('is-error');
          setStatus(msg.error || 'URL fetch failed');
        }
        break;
      }

      case 'searchMentionsResult': {
        if (msg.requestId !== mentionState.requestId) break;
        mentionState.candidates = msg.candidates || [];
        mentionState.index = 0;
        renderMentionMenu();
        break;
      }

      case 'attachFilesResult': {
        pendingRequests.delete(msg.requestId);
        const list = msg.attachments || [];
        for (const a of list) {
          if (!pendingAttachments.some((x) => x.path === a.path)) {
            pendingAttachments.push(a);
          }
        }
        renderAttachRow();
        if (list.length) setStatus(`Attached ${list.length} file(s)`);
        break;
      }

      case 'sessionsUpdate': {
        const liveSessions = (msg.sessions || []).filter((session) => session.status === 'Running');
        const liveIds = new Set(liveSessions.map((session) => session.id));
        for (const id of [...state.tabOrder]) {
          if (!liveIds.has(id)) removeSessionState(id);
        }
        for (const s of liveSessions) {
          ensureSession(s.id, {
            title: state.sessions[s.id]?.title || `Chat ${s.id.slice(-6)}`,
            status: s.status,
            profile: s.profile,
          });
        }
        if (!state.activeId && state.tabOrder.length) {
          state.activeId = state.tabOrder[0];
        }
        persist();
        renderTabs();
        renderMessages();
        syncGeneratingUi();
        break;
      }

      case 'newSession': {
        newSessionPending = false;
        const s = msg.session;
        if (!s) break;
        ensureSession(s.id, {
          title: `Chat ${s.id.slice(-6)}`,
          status: s.status,
          agentMode: s.agent_mode || state.prefs.agentMode || 'agent',
          profile: s.profile || state.prefs.profile || profileForMode(state.prefs.agentMode || 'agent'),
          provider: s.provider,
          model: s.model,
          planContinues: 0,
        });
        state.activeId = s.id;
        persist();
        renderTabs();
        renderMessages();
        syncGeneratingUi();
        const bits = [s.profile || s.agent_mode || 'coding'];
        if (s.model) bits.push(s.model);
        setStatus(`Session ${s.id} · ${bits.join(' · ')}`);
        if (msg.thenSend) {
          let payload = null;
          try {
            payload = JSON.parse(els.input.dataset.pendingSend || 'null');
          } catch {
            payload = null;
          }
          delete els.input.dataset.pendingSend;
          if (payload && (payload.text || payload.attachments?.length)) {
            const session = active();
            if (!session) break;
            addMessage({ role: 'user', content: payload.text || '(attachments)' });
            const assistant = addMessage({
              role: 'assistant',
              content: '',
              streaming: true,
              thinking: { id: uid('think'), text: '', done: false, open: true },
            });
            session.streamingMsgId = assistant.id;
            session.thinkingBlockId = assistant.thinking.id;
            setGenerating(true);
            setStatus('Thinking…');
            vscode.postMessage({
              type: 'sendMessage',
              message: payload.text || '(attachments)',
              sessionId: state.activeId,
              mentions: payload.mentions,
              attachments: payload.attachments,
              urls: payload.urls,
            });
          } else {
            els.input.textContent = msg.thenSend;
            updatePlaceholder();
            send();
          }
        }
        break;
      }

      case 'systemMessage':
        if (state.activeId) addMessage({ role: 'system', content: msg.message });
        setStatus(msg.message || '');
        break;

      case 'userMessage':
        if (state.activeId) addMessage({ role: 'user', content: msg.message });
        break;

      case 'sessionEvent': {
        if (msg.sessionId && msg.sessionId !== state.activeId) break;
        const ev = msg.event || {};
        const session = active();
        if (!session) break;
        let streamingMsgId = session.streamingMsgId;
        // Ignore WS handshake / unknown noise
        if (ev.type === 'connected') break;

        if (ev.type === 'thinking_delta' || ev.ThinkingDelta) {
          const text = ev.text || ev.ThinkingDelta?.text || '';
          appendThinking(text);
        } else if (ev.type === 'text_delta' || ev.TextDelta) {
          const text = ev.text || ev.TextDelta?.text || '';
          if (streamingMsgId) {
            const m = session.messages.find((x) => x.id === streamingMsgId);
            if (m?.thinking && !m.thinking.done) {
              m.thinking.done = true;
              m.thinking.open = false;
              persistSoon();
              schedulePatchActivity(streamingMsgId);
            }
            appendToMessage(streamingMsgId, text);
          } else {
            const m = addMessage({ role: 'assistant', content: text, streaming: true });
            session.streamingMsgId = m.id;
            streamingMsgId = m.id;
          }
        } else if (ev.type === 'status' || ev.Status) {
          const st = (ev.status || ev.Status?.status || '').toString();
          const lower = st.toLowerCase();
          if (lower === 'exited' || lower === 'killed') {
            removeSessionState(session.id);
            renderTabs();
            renderMessages();
            syncGeneratingUi();
          } else if (lower === 'idle' || lower === 'agent_settled' || lower === 'agent_end') {
            const finishedId = streamingMsgId;
            const finished = finishedId
              ? session.messages.find((x) => x.id === finishedId)
              : null;
            if (streamingMsgId) {
              updateMessage(streamingMsgId, {
                streaming: false,
                thinking: { done: true, open: false },
              });
              persist();
            }
            session.stopPending = false;
            setGenerating(false);
            setStatus('');
            session.streamingMsgId = null;
            session.thinkingBlockId = null;
            if (finished) {
              offerSavePlan(finished);
              maybeContinuePlan(finished);
            }
          } else if (lower.includes('think') && streamingMsgId) {
            if (ev.text) appendThinking(String(ev.text));
            setStatus(st);
          } else {
            setStatus(st);
          }
        } else if (ev.type === 'error' || ev.Error) {
          const message = ev.message || ev.Error?.message || 'Harness error';
          if (session.ignoreAbortError && /\b(abort|cancel)/i.test(message)) {
            session.ignoreAbortError = false;
            break;
          }
          if (streamingMsgId) {
            const failed = session.messages.find((x) => x.id === streamingMsgId);
            if (failed) failed._harnessError = true;
            appendToMessage(streamingMsgId, `\n\n⚠️ ${message}`);
            updateMessage(streamingMsgId, {
              streaming: false,
              thinking: { done: true, open: false },
            });
            persist();
          } else {
            addMessage({ role: 'system', content: message });
          }
          session.stopPending = false;
          setGenerating(false);
          setStatus(message);
          session.streamingMsgId = null;
          session.thinkingBlockId = null;
        } else if (ev.type === 'tool_call' || ev.ToolCall) {
          const name = ev.name || ev.ToolCall?.name || 'tool';
          const args = ev.args || ev.ToolCall?.args || '';
          const filePath = toolFilePath(name, args);
          if (streamingMsgId) {
            const m = session.messages.find((x) => x.id === streamingMsgId);
            if (m) {
              m.tools = m.tools || [];
              m.tools.push({
                id: uid('tool'),
                name,
                args,
                path: filePath || undefined,
                open: false,
              });
              // Keep the step list collapsed; only the summary line updates.
              if (m.toolsOpen == null) m.toolsOpen = false;
              const before = (m.changedFiles || []).length;
              recordFileChange(m, name, filePath);
              persistSoon();
              schedulePatchActivity(streamingMsgId);
              if ((m.changedFiles || []).length !== before) patchChanges(streamingMsgId);
            }
          }
        } else if (ev.type === 'tool_result' || ev.ToolResult) {
          const tool = ev.tool || ev.ToolResult?.tool || 'tool';
          const output = ev.output || ev.ToolResult?.output || '';
          if (streamingMsgId) {
            const m = session.messages.find((x) => x.id === streamingMsgId);
            if (m) {
              const before = (m.changedFiles || []).length;
              const existing = (m.tools || []).find((t) => t.name === tool && !t.output);
              if (existing) {
                existing.output = output;
                if (!existing.path) {
                  existing.path = toolFilePath(tool, existing.args) || undefined;
                }
                recordFileChange(m, tool, existing.path);
              } else {
                m.tools = m.tools || [];
                m.tools.push({ id: uid('tool'), name: tool, output, open: false });
              }
              persistSoon();
              schedulePatchActivity(streamingMsgId);
              if ((m.changedFiles || []).length !== before) patchChanges(streamingMsgId);
            }
          }
        }
        break;
      }

      case 'thinkingDelta': {
        appendThinking(msg.text || '');
        break;
      }

      case 'promptAccepted':
        setStatus('Running in harness…');
        // Keep streaming/thinking UI active; events will fill the reply.
        break;

      case 'promptResponse': {
        let text = '';
        if (typeof msg.response === 'string') text = msg.response;
        else if (msg.response?.message) text = msg.response.message;
        else if (msg.response?.note) text = `${msg.response.message || ''}\n${msg.response.note || ''}`.trim();
        else if (msg.response) text = JSON.stringify(msg.response, null, 2);

        const session = active();
        const streamingMsgId = session?.streamingMsgId;
        if (streamingMsgId) {
          const m = session.messages.find((x) => x.id === streamingMsgId);
          if (m) {
            if (!text) {
              text = 'Prompt completed without a response.';
              m._harnessError = true;
            }
            if (!m.content) m.content = text;
            else if (text && !m.content.includes(text)) m.content += (m.content ? '\n' : '') + text;
            m.streaming = false;
            if (m.thinking) {
              m.thinking.done = true;
              m.thinking.open = false;
            }
            persistSoon();
            schedulePatchActivity(streamingMsgId);
            patchBubble(streamingMsgId, { finalize: true });
            session.stopPending = false;
            setGenerating(false);
            setStatus('');
            session.streamingMsgId = null;
            session.thinkingBlockId = null;
            if (!m._harnessError) {
              offerSavePlan(m);
              maybeContinuePlan(m);
            }
            break;
          }
        } else if (text) {
          addMessage({ role: 'assistant', content: text });
        }
        if (session) {
          session.streamingMsgId = null;
          session.thinkingBlockId = null;
          session.stopPending = false;
        }
        setGenerating(false);
        setStatus('');
        break;
      }

      case 'promptError': {
        const session = active();
        const streamingMsgId = session?.streamingMsgId;
        const message = msg.message || 'Prompt failed';
        if (session && streamingMsgId) {
          const failed = session.messages.find((item) => item.id === streamingMsgId);
          if (failed) failed._harnessError = true;
          appendToMessage(streamingMsgId, `\n\n⚠️ ${message}`);
          updateMessage(streamingMsgId, {
            streaming: false,
            thinking: { done: true, open: false },
          });
          session.streamingMsgId = null;
          session.thinkingBlockId = null;
          session.stopPending = false;
        } else {
          addMessage({ role: 'system', content: message });
        }
        setGenerating(false);
        setStatus(message);
        break;
      }

      case 'requestStop':
        stop();
        break;

      case 'stopResult': {
        const session = msg.sessionId ? state.sessions[msg.sessionId] : active();
        if (!session) break;
        session.stopPending = false;
        if (!msg.ok) {
          session.ignoreAbortError = false;
          const response = session.messages.find(
            (message) => message.id === session.streamingMsgId
          );
          if (response) response._stopped = false;
          if (session.id === state.activeId) {
            syncGeneratingUi();
            setStatus(msg.error || 'Unable to stop the active response');
          }
          break;
        }
        const streamingMsgId = session.streamingMsgId;
        if (streamingMsgId) {
          const response = session.messages.find((item) => item.id === streamingMsgId);
          if (response) {
            response.streaming = false;
            if (response.thinking) {
              response.thinking.done = true;
              response.thinking.open = false;
            }
          }
        }
        session.generating = false;
        session.streamingMsgId = null;
        session.thinkingBlockId = null;
        persist();
        if (session.id === state.activeId) {
          renderMessages();
          syncGeneratingUi();
          setStatus('Stopped');
        }
        break;
      }

      case 'planSaved': {
        const session = active();
        const message = session?.messages.find((m) => m.id === msg.messageId);
        if (message) {
          message.plan = msg.plan;
          message.offerSavePlan = false;
          message._planSavePending = false;
        } else if (session) {
          const latest = [...session.messages]
            .reverse()
            .find((m) => m.role === 'assistant');
          if (latest) latest.plan = msg.plan;
        }
        persist();
        renderMessages();
        setStatus(`Saved ${msg.plan.title}`);
        break;
      }

      case 'planSaveFailed': {
        const session = active();
        const message = session?.messages.find((m) => m.id === msg.messageId);
        if (message) {
          message._planSavePending = false;
          message.offerSavePlan = true;
        }
        persist();
        renderMessages();
        setStatus(msg.error || 'Failed to save plan');
        break;
      }

      case 'planUpdated': {
        for (const session of Object.values(state.sessions)) {
          for (const message of session.messages || []) {
            if (message.plan?.id === msg.plan.id) message.plan = msg.plan;
          }
        }
        persist();
        renderMessages();
        setStatus(`Plan ${msg.plan.status}`);
        break;
      }
    }
  });

  // boot
  setMode(state.prefs.agentMode || 'agent', false);
  fillProfiles({});
  fillProviders([]);
  fillModels();
  updatePrefsHint();
  updatePlaceholder();
  renderTabs();
  renderMessages();
  syncGeneratingUi();
  vscode.postMessage({ type: 'ready' });
})();
