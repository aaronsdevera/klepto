(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.KleptoMarkdown = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  function escapeHtml(value) {
    return String(value)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  function inline(source) {
    const tokens = [];
    const token = (html) => {
      const id = `\u0000INLINE${tokens.length}\u0000`;
      tokens.push(html);
      return id;
    };
    let text = String(source || '');
    text = text.replace(/`([^`\n]+)`/g, (_, code) =>
      token(`<code class="md-code">${escapeHtml(code)}</code>`)
    );
    text = text.replace(/\[([^\]\n]+)\]\(([^)\n]+)\)/g, (_, label, target) =>
      token(
        `<a class="md-link" href="#" data-md-link="${encodeURIComponent(
          target.trim()
        )}">${escapeHtml(label)}</a>`
      )
    );
    text = text.replace(/<(https?:\/\/[^>\s]+)>/g, (_, target) =>
      token(
        `<a class="md-link" href="#" data-md-link="${encodeURIComponent(target)}">${escapeHtml(
          target
        )}</a>`
      )
    );
    let html = escapeHtml(text)
      .replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>')
      .replace(/__([^_\n]+)__/g, '<strong>$1</strong>')
      .replace(/~~([^~\n]+)~~/g, '<del>$1</del>')
      .replace(/(^|[^\w])\*([^*\n]+)\*/g, '$1<em>$2</em>')
      .replace(/(^|[^\w])_([^_\n]+)_/g, '$1<em>$2</em>');
    tokens.forEach((value, index) => {
      html = html.replace(`\u0000INLINE${index}\u0000`, value);
    });
    return html;
  }

  function cells(line) {
    return line
      .trim()
      .replace(/^\|/, '')
      .replace(/\|$/, '')
      .split('|')
      .map((cell) => cell.trim());
  }

  function isTableDivider(line) {
    const parts = cells(line);
    return parts.length > 0 && parts.every((part) => /^:?-{3,}:?$/.test(part));
  }

  function render(markdown) {
    const codeBlocks = [];
    let source = String(markdown ?? '')
      .replace(/\r\n?/g, '\n')
      .replace(/```([^\n`]*)\n([\s\S]*?)```/g, (_, language, code) => {
        const id = `\u0000BLOCK${codeBlocks.length}\u0000`;
        const safeLanguage = String(language || '')
          .trim()
          .replace(/[^a-zA-Z0-9_-]/g, '');
        codeBlocks.push(
          `<pre><code${safeLanguage ? ` class="lang-${safeLanguage}"` : ''}>${escapeHtml(
            code.replace(/\n$/, '')
          )}</code></pre>`
        );
        return id;
      });

    const lines = source.split('\n');
    const output = [];
    let paragraph = [];
    let list = null;

    const closeList = () => {
      if (!list) return;
      output.push(`</${list}>`);
      list = null;
    };
    const flushParagraph = () => {
      if (!paragraph.length) return;
      output.push(`<div class="md-p">${paragraph.map(inline).join('<br>')}</div>`);
      paragraph = [];
    };
    const flush = () => {
      flushParagraph();
      closeList();
    };

    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index];
      const block = line.match(/^\u0000BLOCK(\d+)\u0000$/);
      if (block) {
        flush();
        output.push(codeBlocks[Number(block[1])] || '');
        continue;
      }
      if (!line.trim()) {
        flush();
        continue;
      }
      const heading = line.match(/^(#{1,6})\s+(.+)$/);
      if (heading) {
        flush();
        const level = heading[1].length;
        output.push(`<h${level}>${inline(heading[2])}</h${level}>`);
        continue;
      }
      if (/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
        flush();
        output.push('<hr>');
        continue;
      }
      if (
        line.includes('|') &&
        index + 1 < lines.length &&
        isTableDivider(lines[index + 1])
      ) {
        flush();
        const headers = cells(line);
        index += 2;
        const rows = [];
        while (index < lines.length && lines[index].trim() && lines[index].includes('|')) {
          rows.push(cells(lines[index]));
          index += 1;
        }
        index -= 1;
        output.push(
          `<div class="md-table-wrap"><table><thead><tr>${headers
            .map((header) => `<th>${inline(header)}</th>`)
            .join('')}</tr></thead><tbody>${rows
            .map(
              (row) =>
                `<tr>${headers
                  .map((_, cellIndex) => `<td>${inline(row[cellIndex] || '')}</td>`)
                  .join('')}</tr>`
            )
            .join('')}</tbody></table></div>`
        );
        continue;
      }
      const unordered = line.match(/^\s*[-+*]\s+(.+)$/);
      const ordered = line.match(/^\s*\d+[.)]\s+(.+)$/);
      if (unordered || ordered) {
        flushParagraph();
        const type = ordered ? 'ol' : 'ul';
        if (list !== type) {
          closeList();
          output.push(`<${type}>`);
          list = type;
        }
        let item = (unordered || ordered)[1];
        const task = item.match(/^\[([ xX])\]\s+(.+)$/);
        if (task) {
          const checked = task[1].toLowerCase() === 'x';
          item = `<span class="md-task" aria-hidden="true">${checked ? '☑' : '☐'}</span> ${inline(
            task[2]
          )}`;
        } else {
          item = inline(item);
        }
        output.push(`<li>${item}</li>`);
        continue;
      }
      const quote = line.match(/^\s*>\s?(.*)$/);
      if (quote) {
        flush();
        const quoted = [quote[1]];
        while (index + 1 < lines.length) {
          const next = lines[index + 1].match(/^\s*>\s?(.*)$/);
          if (!next) break;
          quoted.push(next[1]);
          index += 1;
        }
        output.push(`<blockquote>${quoted.map(inline).join('<br>')}</blockquote>`);
        continue;
      }
      closeList();
      paragraph.push(line);
    }
    flush();
    return output.join('');
  }

  return { render, escapeHtml };
});
