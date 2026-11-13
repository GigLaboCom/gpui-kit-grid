// An example as a guest in someone else's page: an example's page opened
// with `?embed` (in an iframe), or the module mounted into the page itself
// by `<live-grid>`. Three manners, the same in both:
//
// - The keyboard stays where the reader left it. GPUI's web platform
//   focuses its hidden input (a 1×1 `textarea` in the body) as soon as the
//   window opens, which took the focus from a field of the host page, or
//   gave it to the iframe. `holdFocus` drops those calls until a press in
//   the example; the shell's `?embed` drops GPUI's own focus too.
// - The wheel is the page's at the example's ends. GPUI takes every wheel
//   over its canvas, even one that moves nothing. Here a wheel gesture that
//   starts where the example has no room that way (the module's
//   `scroll_room`) never reaches GPUI, and the browser scrolls the page (or,
//   in an iframe, the page around it). Within a gesture the choice holds,
//   as the browser's own scroll latching does.
// - A copy the page may not make says so. In an iframe without
//   `allow="clipboard-write"` the browser refuses `writeText`, and Ctrl+C
//   did nothing; now a short notice shows over the example.
//
// And it says what it is: a line under it (`creditLine`) carries the
// example's notice, which the market examples need ("Simulated prices, not
// market data": real companies, made-up prices), and a link to the
// licences of what it is built from.

/// The height of the line under an embedded example, in CSS pixels.
export const LINE_HEIGHT = 20;

/// The line under an embedded example: its `notice`, if any, and a link to
/// the `licenses` page, in a new tab. The caller places it.
export function creditLine({ notice, licenses }) {
  const line = document.createElement('div');
  line.className = 'live-grid-credit';
  Object.assign(line.style, {
    boxSizing: 'border-box',
    height: `${LINE_HEIGHT}px`,
    display: 'flex',
    justifyContent: 'flex-end',
    alignItems: 'center',
    gap: '12px',
    padding: '0 8px',
    background: '#121418',
    borderTop: '1px solid #252932',
    color: '#9aa1ae',
    font: '11px/1 system-ui, sans-serif',
    whiteSpace: 'nowrap',
    overflow: 'hidden',
  });
  if (notice) {
    const label = document.createElement('span');
    label.textContent = notice;
    Object.assign(label.style, { overflow: 'hidden', textOverflow: 'ellipsis' });
    line.append(label);
  }
  const link = document.createElement('a');
  link.href = licenses;
  link.target = '_blank';
  link.rel = 'noopener';
  link.textContent = 'Licenses';
  Object.assign(link.style, { color: 'inherit', textDecoration: 'underline' });
  line.append(link);
  return line;
}

const GESTURE_GAP_MS = 200; // wheel events further apart start a new gesture
const NOTICE_MS = 4000;
const NOTICE = 'Copy needs clipboard permission on this site';

// GPUI's hidden input, by the inline style its platform gives it.
const isGpuiInput = (element) =>
  element instanceof HTMLTextAreaElement &&
  element.style.position === 'fixed' &&
  element.style.width === '1px' &&
  element.style.opacity === '0';

const released = new WeakSet(); // GPUI inputs a press has let take the focus
const guests = []; // { root, canvas: () => canvas or null, notice }
let pressing = false; // within a press on a guest's canvas
let lastGuest = null; // the guest pressed last
let focusHeld = false;

/// Drops GPUI's calls to focus its hidden input until the reader presses
/// on that example. Call it before the module's `run`; once per document.
export function holdFocus() {
  if (focusHeld) return;
  focusHeld = true;
  const focus = HTMLElement.prototype.focus;
  HTMLElement.prototype.focus = function (options) {
    if (isGpuiInput(this) && !released.has(this)) {
      if (!pressing) return;
      released.add(this);
    }
    return focus.call(this, options);
  };
  for (const type of ['pointerdown', 'touchend']) {
    // Capture on the window: before GPUI's own listener on its canvas,
    // which focuses the input within the press.
    addEventListener(type, (event) => press(event), { capture: true });
    addEventListener(type, () => (pressing = false));
  }
}

function press(event) {
  const target = event.composedPath()[0];
  const guest = guests.find((guest) => guest.canvas() === target);
  if (!guest) return;
  pressing = true;
  lastGuest = guest;
  setTimeout(() => (pressing = false));
}

/// Makes the module `wasm`, drawing in `canvas()` inside `root`, a guest:
/// wheels at its ends go to the page, and a refused copy shows a notice in
/// `root` (positioned: the notice is placed absolutely in it). `events` is
/// where the wheel is caught on its way to the canvas: `root`, or the
/// element's shadow root.
export function addGuest({ root, events = root, canvas, wasm }) {
  const guest = { root, canvas, notice: null };
  guests.push(guest);
  watchClipboard();
  let gestureEnd = 0;
  let sign = 0;
  let passing = false;
  events.addEventListener(
    'wheel',
    (event) => {
      if (event.composedPath()[0] !== canvas()) return;
      // A sideways wheel and a pinch stay the example's.
      if (event.ctrlKey || Math.abs(event.deltaY) <= Math.abs(event.deltaX)) return;
      const now = event.timeStamp;
      const direction = Math.sign(event.deltaY);
      if (now > gestureEnd || direction !== sign) {
        const room = wasm.scroll_room();
        passing = !(direction < 0 ? room & 1 : room & 2);
        sign = direction;
      }
      gestureEnd = now + GESTURE_GAP_MS;
      if (passing) event.stopPropagation(); // GPUI never sees it
    },
    { capture: true, passive: true },
  );
  return guest;
}

let clipboardWatched = false;

// Wraps the clipboard's `writeText`, which GPUI copies through: a refusal
// while a GPUI input has the keyboard shows the notice in the guest pressed
// last. The page's own copies pass through untouched.
function watchClipboard() {
  if (clipboardWatched || !navigator.clipboard?.writeText) return;
  clipboardWatched = true;
  const clipboard = navigator.clipboard;
  const writeText = clipboard.writeText.bind(clipboard);
  clipboard.writeText = (text) => {
    const ours = isGpuiInput(document.activeElement);
    const written = writeText(text);
    if (ours && lastGuest) {
      const guest = lastGuest;
      written.catch((error) => {
        console.warn(`copy refused: ${error}`);
        showNotice(guest);
      });
    }
    return written;
  };
}

function showNotice(guest) {
  if (!guest.notice) {
    const notice = document.createElement('div');
    notice.setAttribute('role', 'status');
    notice.textContent = NOTICE;
    Object.assign(notice.style, {
      position: 'absolute',
      left: '50%',
      bottom: `${LINE_HEIGHT + 8}px`,
      transform: 'translateX(-50%)',
      maxWidth: 'calc(100% - 24px)',
      padding: '6px 12px',
      borderRadius: '6px',
      background: 'rgba(20, 22, 26, 0.92)',
      color: '#f2f3f5',
      font: '13px/1.4 system-ui, sans-serif',
      boxShadow: '0 2px 8px rgba(0, 0, 0, 0.3)',
      pointerEvents: 'none',
      zIndex: '1',
    });
    guest.notice = notice;
  }
  guest.root.append(guest.notice);
  clearTimeout(guest.noticeTimer);
  guest.noticeTimer = setTimeout(() => guest.notice.remove(), NOTICE_MS);
}
