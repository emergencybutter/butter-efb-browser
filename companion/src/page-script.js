/**
 * Script injected in every document (all frames) of the remote browser.
 * It reports focus changes on editable elements and intercepts <select>
 * elements, whose native popup is not rendered in headless screencasts.
 *
 * It communicates through bindings exposed by the companion:
 *   window.__efbFocus({ editable, inputType, value })
 *   window.__efbSelect({ options: [{ label, disabled }], selectedIndex })
 */
export const PAGE_SCRIPT = `(() => {
  if (window.__efbInstalled) return;
  window.__efbInstalled = true;

  const NON_TEXT_INPUTS = new Set(['button', 'checkbox', 'radio', 'submit', 'reset', 'file', 'image', 'range', 'color', 'hidden']);

  const deepActive = () => {
    let el = document.activeElement;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    return el;
  };

  const isEditable = (el) => {
    if (!el) return false;
    if (el.isContentEditable) return true;
    if (el.tagName === 'TEXTAREA') return !el.readOnly && !el.disabled;
    if (el.tagName === 'INPUT') return !NON_TEXT_INPUTS.has((el.type || 'text').toLowerCase()) && !el.readOnly && !el.disabled;
    return false;
  };

  const report = () => {
    const el = deepActive();
    const editable = isEditable(el);
    try {
      window.__efbFocus && window.__efbFocus({
        editable,
        inputType: editable && el.tagName === 'INPUT' ? (el.type || 'text') : (editable ? 'text' : ''),
        value: editable && 'value' in el ? String(el.value).slice(0, 2000) : '',
      });
    } catch (e) {}
  };

  document.addEventListener('focusin', report, true);
  document.addEventListener('focusout', () => setTimeout(report, 0), true);

  // <select> elements: open our own picker on the EFB instead of the native popup.
  document.addEventListener('mousedown', (ev) => {
    const path = ev.composedPath ? ev.composedPath() : [ev.target];
    const sel = path.find((n) => n && n.tagName === 'SELECT');
    if (!sel || sel.disabled || sel.multiple) return;
    ev.preventDefault();
    sel.focus();
    window.__efbActiveSelect = sel;
    try {
      window.__efbSelect && window.__efbSelect({
        options: Array.from(sel.options).map((o) => ({ label: o.label || o.text, disabled: o.disabled })),
        selectedIndex: sel.selectedIndex,
      });
    } catch (e) {}
  }, true);

  window.__efbApplySelect = (index) => {
    const sel = window.__efbActiveSelect;
    if (!sel) return false;
    window.__efbActiveSelect = null;
    if (index >= 0 && index < sel.options.length && sel.selectedIndex !== index) {
      sel.selectedIndex = index;
      sel.dispatchEvent(new Event('input', { bubbles: true }));
      sel.dispatchEvent(new Event('change', { bubbles: true }));
    }
    return true;
  };
})();`;
