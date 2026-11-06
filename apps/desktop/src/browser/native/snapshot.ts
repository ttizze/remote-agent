(() => {
    // This runs in WKContentWorld BexBrowser. Page scripts cannot replace the
    // reference table or read imported credentials through the tool snapshot.
    const refs = new Map<string, BexBrowserElement>(), lines: string[] = [], secrets: string[] = [];
    const token = crypto.randomUUID();
    let next = 0, visited = 0;
    function recordElement(e: BexBrowserElement, style: CSSStyleDeclaration & { webkitTextSecurity?: string }) {
        const password = e.type === 'password' || (style.webkitTextSecurity && style.webkitTextSecurity !== 'none');
        if (password && e.value) secrets.push(e.value);
        const role = e.getAttribute('role') || ({A:'link', BUTTON:'button', INPUT:e.type === 'checkbox' ? 'checkbox' : 'textbox', TEXTAREA:'textbox', SELECT:'combobox', H1:'heading', H2:'heading', H3:'heading', IMG:'image'} as Record<string, string>)[e.tagName];
        if (role) {
            const ref = `ref=${token}:${++next}`;
            refs.set(ref,e);
            const label = e.getAttribute('aria-label') || e.getAttribute('alt') || e.getAttribute('placeholder') || (e.labels ? Array.from(e.labels).map(l=>l.textContent).join(' ') : '') || e.textContent || '';
            const value = password ? '[protected]' : ('value' in e && e.tagName !== 'BUTTON' ? e.value : '');
            lines.push(`${ref} ${role} ${label.trim().slice(0,500)}${value ? ' value='+value.slice(0,1000) : ''}`);
        } else {
            const text = Array.from(e.childNodes).filter(n=>n.nodeType===Node.TEXT_NODE).map(n=>n.textContent!.trim()).filter(Boolean).join(' ');
            if (text) lines.push(text.slice(0,1000));
        }
    }
    function visit(root: ParentNode, depth: number) {
        if (depth > 20 || lines.join('\n').length > 20000) return;
        for (const child of root.children || []) {
            const e = child as BexBrowserElement;
            if (++visited > 5000 || lines.length >= 500) return;
            const style = getComputedStyle(e);
            if (style.display === 'none' || style.visibility === 'hidden' || e.tagName === 'SCRIPT' || e.tagName === 'STYLE') continue;
            recordElement(e, style);
            if (e.shadowRoot) visit(e.shadowRoot,depth+1);
            if (e.tagName === 'IFRAME') { try { if (e.contentDocument) visit(e.contentDocument,depth+1); } catch {} }
            visit(e,depth+1);
        }
    }
    visit(document,0);
    globalThis.bexReferences = refs;
    globalThis.bexReferenceURL = location.href;
    let text = lines.join('\n').slice(0,20000);
    for (const secret of secrets) text = text.split(secret).join('[protected]');
    return text;
})()
