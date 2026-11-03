(() => {
  const refs = new WeakMap(); let nextRef = 1; let annotating = false; let selection = null;
  const safeText = (node) => (node?.innerText || node?.textContent || '').replace(/\s+/g, ' ').trim();
  const selectorFor = (el) => {
    if (el.id) return `#${CSS.escape(el.id)}`;
    const parts = []; let n = el;
    while (n && n.nodeType === 1 && parts.length < 5) {
      let part = n.localName;
      if (n.parentElement) { const siblings = [...n.parentElement.children].filter(x => x.localName === n.localName); if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(n) + 1})`; }
      parts.unshift(part); n = n.parentElement;
    }
    return parts.join(' > ');
  };
  const visible = el => { const r = el.getBoundingClientRect(), s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  const elements = () => [...document.querySelectorAll('a,button,input,textarea,select,[role], [tabindex]')].filter(el => visible(el) && !['password','hidden'].includes((el.type || '').toLowerCase())).slice(0, 200);
  const ref = el => { if (!refs.has(el)) refs.set(el, String(nextRef++)); return refs.get(el); };
  const snapshot = (drain = false) => {
    const candidates = elements();
    const body = safeText(document.body).slice(0, 24000);
    const result = { url: location.href, title: document.title, text: body, refs: candidates.map(el => ({id:ref(el),tag:el.localName,role:el.getAttribute('role') || (el.localName === 'a' ? 'link' : el.localName === 'button' ? 'button' : el.localName),label:(el.getAttribute('aria-label') || el.getAttribute('placeholder') || safeText(el) || el.getAttribute('name') || '').slice(0,180)})), annotating, ready: document.readyState === 'complete' };
    if (selection && drain) { result.selection = selection; selection = null; }
    return result;
  };
  const resolve = id => elements().find(el => ref(el) === String(id));
  document.addEventListener('click', e => {
    if (!annotating) return; const el = e.target?.closest?.('a,button,input,textarea,select,[role],p,h1,h2,h3,li,div,span'); if (!el) return;
    e.preventDefault(); e.stopImmediatePropagation(); const r = el.getBoundingClientRect();
    document.querySelectorAll('[data-brr-highlight]').forEach(n => n.removeAttribute('data-brr-highlight'));
    el.setAttribute('data-brr-highlight',''); selection = {url:location.href,title:document.title,selector:selectorFor(el),quote:safeText(el).slice(0,500),x:r.x,y:r.y,width:r.width,height:r.height};
  }, true);
  let accent = '#dbff00';
  const style = document.createElement('style');
  const mountStyle = () => { const parent = document.head || document.documentElement; if (parent) parent.append(style); };
  const updateStyle = () => { style.textContent = `[data-brr-highlight]{outline:3px solid ${accent}!important;outline-offset:2px!important;cursor:crosshair!important}`; };
  updateStyle();
  if (document.documentElement) mountStyle(); else document.addEventListener('DOMContentLoaded', mountStyle, {once:true});
  Object.defineProperty(window, '__brrBrowser', { configurable:false, value: {
    snapshot: drain => snapshot(Boolean(drain)),
    annotate: (enabled, color) => { annotating=!!enabled; if (typeof color === 'string' && CSS.supports('color', color)) { accent=color; updateStyle(); } return snapshot(false); },
    action: data => { const el=data.ref == null ? null : resolve(data.ref); if (data.action==='scroll') { (el || document.scrollingElement || document.documentElement).scrollBy({top:Number(data.deltaY)||0,behavior:'instant'}); return snapshot(false); } if (!el) throw new Error('Unknown element reference');
      if (data.action==='click') { el.click(); }
      else if (data.action==='fill') { if (!['INPUT','TEXTAREA'].includes(el.tagName) || (el.type||'').toLowerCase()==='password') throw new Error('Reference is not a fillable text field'); const setter=Object.getOwnPropertyDescriptor(el.tagName==='INPUT'?HTMLInputElement.prototype:HTMLTextAreaElement.prototype,'value').set; setter.call(el,String(data.text||'')); el.dispatchEvent(new Event('input',{bubbles:true})); el.dispatchEvent(new Event('change',{bubbles:true})); }
      else if (data.action==='press') { el.focus(); const key=String(data.key||''); const proceed=el.dispatchEvent(new KeyboardEvent('keydown',{key,bubbles:true,cancelable:true})); if (proceed && key==='Enter') { if (el.tagName==='BUTTON' || el.tagName==='A') el.click(); else if (el.form && el.tagName==='INPUT') el.form.requestSubmit(); } el.dispatchEvent(new KeyboardEvent('keyup',{key,bubbles:true})); }
      else if (data.action==='scroll') { el.scrollBy({top:Number(data.deltaY)||0,behavior:'instant'}); }
      else throw new Error('Unsupported action'); return snapshot(false);
    }
  }});
})();
