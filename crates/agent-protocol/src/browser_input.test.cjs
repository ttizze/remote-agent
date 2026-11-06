const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

const source = readFileSync(`${__dirname}/browser_input.generated.js`, 'utf8');
const observe = activeElement => vm.runInNewContext(source, {
    document: { activeElement }, getComputedStyle: element => element.style || {},
});

test('opens text keyboards only for editable text controls', () => {
    for (const type of ['text', 'search', 'email', 'url', 'tel', 'number']) {
        assert.equal(observe({ tagName: 'INPUT', type }), 'Text', type);
    }
    assert.equal(observe({ tagName: 'TEXTAREA' }), 'Text');
    assert.equal(observe({ tagName: 'SPAN', isContentEditable: true }), 'Text');
    for (const type of ['button', 'checkbox', 'radio', 'date', 'file', 'range', 'hidden']) {
        assert.equal(observe({ tagName: 'INPUT', type }), null, type);
    }
    for (const element of [null, { tagName: 'BODY' }, { tagName: 'SELECT' },
        { tagName: 'INPUT', type: 'text', disabled: true },
        { tagName: 'TEXTAREA', readOnly: true }]) {
        assert.equal(observe(element), null);
    }
});

test('keeps passwords and visually secured editors masked without reading their contents', () => {
    for (const editor of [
        { tagName: 'INPUT', type: 'password' },
        { tagName: 'INPUT', type: 'text', style: { webkitTextSecurity: 'disc' } },
        { tagName: 'DIV', isContentEditable: true, style: { webkitTextSecurity: 'circle' } },
    ]) {
        Object.defineProperty(editor, 'value', { get() { throw new Error('must not read values'); } });
        Object.defineProperty(editor, 'textContent', { get() { throw new Error('must not read text'); } });
        assert.equal(observe(editor), 'Password');
    }
    assert.equal(observe({ tagName: 'INPUT', type: 'text', style: { webkitTextSecurity: 'none' } }), 'Text');
});

test('follows focused editors inside shadow roots and accessible frames', () => {
    const editor = { tagName: 'INPUT', type: 'password' };
    assert.equal(observe({ shadowRoot: { activeElement: { contentDocument: { activeElement: editor } } } }), 'Password');
    const inaccessible = { tagName: 'IFRAME' };
    Object.defineProperty(inaccessible, 'contentDocument', { get() { throw new Error('cross origin'); } });
    assert.equal(observe(inaccessible), null);
    let nested = editor;
    for (let depth = 0; depth < 31; depth++) nested = { shadowRoot: { activeElement: nested } };
    assert.equal(observe(nested), 'Password');
    assert.equal(observe({ shadowRoot: { activeElement: nested } }), null);
});
