(() => {
  const vscode = acquireVsCodeApi();
  const els = {
    title: document.getElementById('title'),
    overview: document.getElementById('overview'),
    state: document.getElementById('state'),
    error: document.getElementById('error'),
    todos: document.getElementById('todos'),
    todoHeading: document.getElementById('todoHeading'),
    agents: document.getElementById('agents'),
    agentHeading: document.getElementById('agentHeading'),
    content: document.getElementById('content'),
    source: document.getElementById('source'),
    build: document.getElementById('build'),
  };
  let plan = null;
  let sessions = [];
  let building = false;

  const escapeHtml = (value) =>
    String(value || '')
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');

  function renderMarkdown(markdown) {
    let text = escapeHtml(markdown);
    const blocks = [];
    text = text.replace(/```([\s\S]*?)```/g, (_, code) => {
      const token = `@@BLOCK_${blocks.length}@@`;
      blocks.push(`<pre><code>${code.trim()}</code></pre>`);
      return token;
    });
    text = text
      .replace(/^### (.+)$/gm, '<h3>$1</h3>')
      .replace(/^## (.+)$/gm, '<h2>$1</h2>')
      .replace(/^# (.+)$/gm, '<h1>$1</h1>')
      .replace(/`([^`\n]+)`/g, '<code>$1</code>')
      .replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, label, target) => {
        const encoded = encodeURIComponent(
          target.replaceAll('&quot;', '"').replaceAll('&amp;', '&')
        );
        return `<a data-path="${encoded}">${label}</a>`;
      })
      .replace(/^\s*[-*] (.+)$/gm, '<li>$1</li>')
      .replace(/(<li>.*<\/li>\n?)+/g, '<ul>$&</ul>')
      .replace(/\n{2,}/g, '</p><p>')
      .replace(/\n/g, '<br>');
    blocks.forEach((block, index) => {
      text = text.replace(`@@BLOCK_${index}@@`, block);
    });
    return `<p>${text}</p>`;
  }

  function nextTodoStatus(status) {
    if (status === 'pending') return 'in_progress';
    if (status === 'in_progress') return 'completed';
    return 'pending';
  }

  function render() {
    if (!plan) return;
    els.title.textContent = plan.title || 'Plan';
    els.overview.textContent = plan.overview || '';
    els.state.textContent = `${plan.status} · revision ${plan.revision}`;
    els.todoHeading.textContent = `${plan.todos?.length || 0} To-dos`;
    els.todos.replaceChildren();
    for (const todo of plan.todos || []) {
      const row = document.createElement('div');
      row.className = `todo ${todo.status}`;
      const marker = document.createElement('button');
      marker.className = 'todo-marker';
      marker.title = `Mark ${nextTodoStatus(todo.status).replace('_', ' ')}`;
      marker.textContent = todo.status === 'completed' ? '✓' : todo.status === 'in_progress' ? '•' : '';
      marker.addEventListener('click', () => {
        marker.disabled = true;
        vscode.postMessage({
          type: 'updateTodo',
          todoId: todo.id,
          status: nextTodoStatus(todo.status),
        });
      });
      const content = document.createElement('div');
      content.className = 'todo-content';
      content.textContent = todo.content;
      row.append(marker, content);
      els.todos.appendChild(row);
    }
    if (!plan.todos?.length) {
      els.todos.innerHTML = '<div class="empty">No structured to-dos in this plan.</div>';
    }

    const sessionById = new Map(sessions.map((session) => [session.id, session]));
    els.agentHeading.textContent = `Referenced by ${plan.agents?.length || 0} Agent${
      plan.agents?.length === 1 ? '' : 's'
    }`;
    els.agents.replaceChildren();
    for (const agent of plan.agents || []) {
      const session = sessionById.get(agent.session_id);
      const row = document.createElement('div');
      row.className = 'agent';
      row.innerHTML =
        `<span class="agent-icon">✣</span><div class="agent-main">` +
        `<div>${escapeHtml(agent.label || agent.session_id)}</div>` +
        `<div class="agent-meta">${escapeHtml(agent.role)} · ${
          agent.todo_ids?.length || 0
        } todos assigned</div></div>` +
        `<span class="agent-status">${escapeHtml(session?.status || 'Recorded')}</span>`;
      els.agents.appendChild(row);
    }
    if (!plan.agents?.length) {
      els.agents.innerHTML = '<div class="empty">No agent sessions reference this plan yet.</div>';
    }

    els.content.innerHTML = renderMarkdown(plan.content || '');
    els.content.querySelectorAll('a[data-path]').forEach((link) => {
      link.addEventListener('click', () => {
        vscode.postMessage({
          type: 'openFile',
          path: decodeURIComponent(link.dataset.path || ''),
        });
      });
    });
    els.build.disabled =
      building || plan.status === 'building' || plan.status === 'completed' || plan.status === 'rejected';
    els.build.textContent = building || plan.status === 'building' ? 'Building…' : 'Build';
  }

  function showError(error) {
    els.error.textContent = error;
    els.error.classList.toggle('visible', Boolean(error));
  }

  els.source.addEventListener('click', () => vscode.postMessage({ type: 'openSource' }));
  els.build.addEventListener('click', () => {
    showError('');
    vscode.postMessage({ type: 'build' });
  });
  window.addEventListener('message', (event) => {
    const message = event.data;
    if (message.type === 'plan') {
      plan = message.plan;
      sessions = message.sessions || [];
      showError('');
      render();
    } else if (message.type === 'building') {
      building = Boolean(message.value);
      render();
    } else if (message.type === 'error') {
      showError(message.error || 'Plan operation failed');
      building = false;
      render();
    }
  });
  vscode.postMessage({ type: 'ready' });
})();
