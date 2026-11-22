/// <reference types="@microsoft/msfs-types/js/common" />

let counter = 0;

/** Last time the user pressed the mouse (inside the EFB, or outside it in the cockpit). */
let lastPointerAt = 0;
document.addEventListener('mousedown', () => (lastPointerAt = Date.now()), true);

const ENTER = 13;
const ESCAPE = 27;
const SUBMIT_DEDUPE_MS = 400;
const POINTER_BLUR_WINDOW_MS = 400;

export interface SimInputOptions {
  /** Text entered with the OS / VR keyboard. Defaults to setting the field value. */
  onOsText?: (text: string) => void;
  /**
   * Called once when the user validates the field with Enter. Enter is detected on
   * keydown, keypress and keyup, because the sim does not always deliver all of them.
   */
  onSubmit?: () => void;
  /**
   * Also submit when the field loses focus without a mouse press or Escape. In the sim,
   * Enter sometimes just unfocuses the field without any key event reaching the page.
   */
  submitOnKeyboardBlur?: boolean | (() => boolean);
}

/**
 * Makes a raw <input> usable inside the sim: while focused, the sim stops
 * consuming keyboard events (FOCUS_INPUT_FIELD), clicking outside the EFB
 * blurs it, and text typed on the OS / VR keyboard is delivered.
 * Mirrors what the efb-api Input component does.
 *
 * @returns a cleanup function.
 */
export function bindSimInput(el: HTMLInputElement, options: SimInputOptions = {}): () => void {
  if (!el.id) el.id = `buttersim-browser-input-${++counter}`;
  let focused = false;
  let lastSubmitAt = 0;
  let cancelled = false;

  const submit = (): void => {
    const now = Date.now();
    if (now - lastSubmitAt < SUBMIT_DEDUPE_MS) return;
    lastSubmitAt = now;
    if (options.onSubmit) options.onSubmit();
  };

  const onOs = (text: string): void => {
    if (options.onOsText) {
      options.onOsText(text);
    } else {
      el.value = text;
      el.dispatchEvent(new Event('input'));
      el.dispatchEvent(new Event('change'));
    }
    lastSubmitAt = Date.now(); // the OS keyboard already validated: don't submit again on blur
    el.blur();
  };
  const onOutside = (): void => {
    lastPointerAt = Date.now();
    el.blur();
  };

  const onKey = (e: KeyboardEvent): void => {
    const code = e.keyCode || e.which;
    if (code === ENTER || e.key === 'Enter') {
      e.preventDefault();
      submit();
    } else if (code === ESCAPE) {
      cancelled = true;
    }
  };

  const onFocus = (): void => {
    if (focused) return;
    focused = true;
    cancelled = false;
    Coherent.trigger('FOCUS_INPUT_FIELD', el.id, '', '', '', false);
    Coherent.on('mousePressOutsideView', onOutside);
    Coherent.on('SetInputTextFromOS', onOs);
  };
  const onBlur = (): void => {
    if (!focused) return;
    focused = false;
    Coherent.trigger('UNFOCUS_INPUT_FIELD', el.id);
    Coherent.off('mousePressOutsideView', onOutside);
    Coherent.off('SetInputTextFromOS', onOs);
    const byPointer = Date.now() - lastPointerAt < POINTER_BLUR_WINDOW_MS;
    const opt = options.submitOnKeyboardBlur;
    const allowed = typeof opt === 'function' ? opt() : !!opt;
    if (allowed && !byPointer && !cancelled) submit();
  };

  el.addEventListener('focus', onFocus);
  el.addEventListener('blur', onBlur);
  if (options.onSubmit) {
    el.addEventListener('keydown', onKey);
    el.addEventListener('keypress', onKey);
    el.addEventListener('keyup', onKey);
  }
  return (): void => {
    onBlur();
    el.removeEventListener('focus', onFocus);
    el.removeEventListener('blur', onBlur);
    el.removeEventListener('keydown', onKey);
    el.removeEventListener('keypress', onKey);
    el.removeEventListener('keyup', onKey);
  };
}
