type BexBrowserElement = HTMLElement & Partial<Pick<HTMLInputElement, 'type' | 'value' | 'labels' | 'disabled' | 'readOnly'>>
  & Partial<Pick<HTMLIFrameElement, 'contentDocument'>>;
declare var bexReferences: Map<string, BexBrowserElement> | undefined;
declare var bexReferenceURL: string | undefined;
