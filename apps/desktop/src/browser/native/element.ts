(selector: string, operation: string, value: string) => {
    let e: BexBrowserElement | undefined;
    if (selector.startsWith('ref=')) {
        if (globalThis.bexReferenceURL !== location.href) throw new Error('Stale page reference');
        e = globalThis.bexReferences?.get(selector);
    } else {
        const nodes = document.querySelectorAll(selector);
        if (nodes.length !== 1) throw new Error('Expected exactly one element');
        e = nodes[0] as BexBrowserElement;
    }
    if (!e?.isConnected || e.disabled) throw new Error('Element unavailable');
    if (operation === 'select') {
        if (e.tagName !== 'SELECT') throw new Error('Expected enabled select');
        const option = Array.from((e as HTMLSelectElement).options).find(o => (o.value === value || o.label === value) && !o.disabled && !(o.parentElement as HTMLOptGroupElement).disabled);
        if (!option) throw new Error('Missing option');
        e.value = option.value;
        e.dispatchEvent(new Event('input', {bubbles:true}));
        e.dispatchEvent(new Event('change', {bubbles:true}));
        return true;
    }
    if (operation === 'fill' && (e.readOnly || !(e.isContentEditable || e.tagName === 'TEXTAREA' || (e.tagName === 'INPUT' && ['text','email','password','search','tel','url','number'].includes(e.type!))))) throw new Error('Not editable');
    if (operation === 'focused') {
        if ((e.getRootNode() as Document | ShadowRoot).activeElement !== e) throw new Error('Field did not receive focus');
        return true;
    }
    e.scrollIntoView({block:'center',inline:'center'});
    let view: Window = e.ownerDocument.defaultView!;
    const r = e.getBoundingClientRect();
    const left = Math.max(0,r.left), top = Math.max(0,r.top), right = Math.min(view.innerWidth,r.right), bottom = Math.min(view.innerHeight,r.bottom);
    if (left >= right || top >= bottom || view.getComputedStyle(e).visibility === 'hidden') throw new Error('Not visible');
    let x=(left+right)/2, y=(top+bottom)/2;
    const hit = (e.getRootNode() as Document).elementFromPoint(x,y);
    if (hit !== e && !e.contains(hit)) throw new Error('Element is covered');
    while (view !== view.top) {
        const f=view.frameElement as HTMLElement | null;
        if (!f) throw new Error('Frame unavailable');
        const b=f.getBoundingClientRect();
        x=b.left+(f.clientLeft+x)*b.width/f.offsetWidth;
        y=b.top+(f.clientTop+y)*b.height/f.offsetHeight;
        view=view.parent;
    }
    return {x,y};
}
