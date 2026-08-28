const { strict: assert } = require('assert');
const { describe, it } = require('node:test');
const { render } = require('../media/markdown.js');

describe('chat markdown renderer', () => {
  it('renders links, headings, ordered and unordered lists, and emphasis', () => {
    const html = render(`# Klepto

Klepto wraps [pi](https://pi.dev).

1. First component
2. Second component

Key features:
- **Code indexing**
- _Provider management_`);
    assert.match(html, /<h1>Klepto<\/h1>/);
    assert.match(html, /data-md-link="https%3A%2F%2Fpi.dev"/);
    assert.doesNotMatch(html, /\[pi\]/);
    assert.match(html, /<ol>/);
    assert.match(html, /<ul>/);
    assert.match(html, /<strong>Code indexing<\/strong>/);
    assert.match(html, /<em>Provider management<\/em>/);
  });

  it('renders code, task lists, blockquotes, and tables while escaping HTML', () => {
    const html = render(`> Important

- [x] Done
- [ ] Pending

| Name | State |
| --- | --- |
| plan | ready |

\`\`\`html
<script>alert("no")</script>
\`\`\``);
    assert.match(html, /<blockquote>Important<\/blockquote>/);
    assert.match(html, /☑/);
    assert.match(html, /☐/);
    assert.match(html, /<table>/);
    assert.match(html, /&lt;script&gt;/);
    assert.doesNotMatch(html, /<script>alert/);
  });
});
