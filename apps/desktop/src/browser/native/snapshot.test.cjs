const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const vm = require('node:vm');

const source = readFileSync(`${__dirname}/snapshot.generated.js`, 'utf8');

function element(tagName, { attributes = {}, text = '', children = [], ...properties } = {}) {
    return {
        tagName, children, childNodes: [], textContent: text,
        getAttribute: name => attributes[name] || null,
        ...properties,
    };
}

function snapshot(children) {
    const context = vm.createContext({
        document: { children }, location: { href: 'https://example.test/page' },
        crypto: { randomUUID: () => 'snapshot-token' }, Node: { TEXT_NODE: 3 },
        getComputedStyle: e => ({ display: 'block', visibility: 'visible', ...e.style }),
    });
    return { text: vm.runInContext(source, context), context };
}

test('keeps document, shadow and accessible frame order with exact element references', () => {
    const heading = element('H1', { text: ' Heading ' });
    const button = element('BUTTON', { text: 'Ignored', value: 'not a button label', attributes: { 'aria-label': ' Submit ' } });
    const shadow = element('INPUT', { type: 'checkbox', value: 'on' });
    const frame = element('A', { text: ' Frame link ' });
    const light = element('TEXTAREA', { value: 'draft', labels: [{ textContent: 'Editor' }] });
    const container = element('DIV', {
        childNodes: [{ nodeType: 3, textContent: ' Plain text ' }, { nodeType: 1, textContent: 'excluded' }],
        shadowRoot: { children: [shadow] }, children: [light],
    });
    const inaccessible = element('IFRAME');
    Object.defineProperty(inaccessible, 'contentDocument', { get() { throw new Error('cross origin'); } });
    const result = snapshot([
        heading, button, container, element('IFRAME', { contentDocument: { children: [frame] } }), inaccessible,
        element('BUTTON', { text: 'hidden', style: { visibility: 'hidden' } }),
        element('BUTTON', { text: 'not displayed', style: { display: 'none' } }),
        element('SCRIPT', { text: 'script' }), element('STYLE', { text: 'style' }),
    ]);
    assert.equal(result.text, [
        'ref=snapshot-token:1 heading Heading',
        'ref=snapshot-token:2 button Submit',
        'Plain text',
        'ref=snapshot-token:3 checkbox  value=on',
        'ref=snapshot-token:4 textbox Editor value=draft',
        'ref=snapshot-token:5 link Frame link',
    ].join('\n'));
    assert.deepEqual(Array.from(result.context.bexReferences.values()), [heading, button, shadow, light, frame]);
    assert.equal(result.context.bexReferenceURL, 'https://example.test/page');
});

test('protects password and visually secured values everywhere in the returned text', () => {
    const result = snapshot([
        element('INPUT', { type: 'password', value: 'fixture-secret', attributes: { placeholder: 'Password' } }),
        element('INPUT', { type: 'text', value: 'masked-secret', style: { webkitTextSecurity: 'disc' } }),
        element('BUTTON', { text: 'fixture-secret masked-secret' }),
        element('INPUT', { type: 'text', value: 'public', style: { webkitTextSecurity: 'none' } }),
    ]);
    assert.equal(result.text, [
        'ref=snapshot-token:1 textbox Password value=[protected]',
        'ref=snapshot-token:2 textbox  value=[protected]',
        'ref=snapshot-token:3 button [protected] [protected]',
        'ref=snapshot-token:4 textbox  value=public',
    ].join('\n'));
});

test('preserves role and label precedence and per-field truncation', () => {
    const label = 'x'.repeat(501), value = 'v'.repeat(1001);
    const result = snapshot([
        element('A', { attributes: { role: 'menuitem', 'aria-label': 'aria', alt: 'alt', placeholder: 'placeholder' }, text: 'text' }),
        element('IMG', { attributes: { alt: 'alt', placeholder: 'placeholder' } }),
        element('INPUT', { attributes: { placeholder: label }, value }),
        element('DIV', { childNodes: [{ nodeType: 3, textContent: 't'.repeat(1001) }] }),
    ]);
    assert.equal(result.text, [
        'ref=snapshot-token:1 menuitem aria',
        'ref=snapshot-token:2 image alt',
        `ref=snapshot-token:3 textbox ${'x'.repeat(500)} value=${'v'.repeat(1000)}`,
        't'.repeat(1000),
    ].join('\n'));
});

test('keeps depth, element, line and returned-text budgets without truncating the reference table early', () => {
    let nested = element('BUTTON', { text: '22' });
    for (let depth = 21; depth > 0; depth--) nested = element('BUTTON', { text: String(depth), children: [nested] });
    const deep = snapshot([nested]);
    assert.equal(deep.context.bexReferences.size, 21);
    assert.equal(deep.text.split('\n').at(-1), 'ref=snapshot-token:21 button 21');
    const hidden = Array.from({ length: 5000 }, () => element('DIV', { style: { display: 'none' } }));
    assert.equal(snapshot([...hidden.slice(1), element('BUTTON', { text: 'last allowed element' })]).text,
        'ref=snapshot-token:1 button last allowed element');
    assert.equal(snapshot([...hidden, element('BUTTON', { text: 'beyond visited budget' })]).text, '');
    const many = snapshot(Array.from({ length: 501 }, () => element('BUTTON', { text: 'row' })));
    assert.equal(many.text.split('\n').length, 500);
    assert.equal(many.context.bexReferences.size, 500);
    const long = snapshot(Array.from({ length: 30 }, (_, index) => element('INPUT', {
        value: 'v'.repeat(1000), children: index === 29 ? [element('BUTTON', { text: 'beyond text budget' })] : [],
    })));
    assert.equal(long.text.length, 20000);
    assert.equal(long.context.bexReferences.size, 30);
});
