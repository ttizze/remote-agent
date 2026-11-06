(() => {
    type FocusedElement = Element & { disabled?: boolean; readOnly?: boolean; type?: string; isContentEditable?: boolean; contentDocument?: Document | null };
    let element = document.activeElement as FocusedElement | null;
    for (let depth = 0; element && depth < 32; depth++) {
        let nested;
        try {
            nested = element.shadowRoot?.activeElement || element.contentDocument?.activeElement;
        } catch (_) {
            // Cross-origin frames remain operable through the manual keyboard button.
            return null;
        }
        if (nested) {
            element = nested as FocusedElement;
            continue;
        }
        if (element.disabled || element.readOnly) return null;
        const textInput = element.tagName === 'INPUT' &&
            ['text', 'search', 'email', 'url', 'tel', 'number', 'password'].includes(element.type!);
        if (!textInput && element.tagName !== 'TEXTAREA' && !element.isContentEditable) return null;
        const security = (getComputedStyle(element) as CSSStyleDeclaration & { webkitTextSecurity?: string }).webkitTextSecurity;
        return element.type === 'password' || (security && security !== 'none') ? 'Password' : 'Text';
    }
    return null;
})()
